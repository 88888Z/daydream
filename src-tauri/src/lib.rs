mod config;
mod idle;
mod player;
mod thumbnail;

use config::ConfigState;
use player::MpvPlayer;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{
    AppHandle, Emitter, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

// ── Shared atomic state ──────────────────────────────────────────────

static IDLE_MONITOR_ACTIVE: AtomicBool = AtomicBool::new(false);
static MONITOR_GEN: AtomicU64 = AtomicU64::new(0);
pub static IS_PLAYING: AtomicBool = AtomicBool::new(false);
pub static LAST_PLAYED_UUID: Mutex<Option<String>> = Mutex::new(None);
pub static MPV_JUST_TRANSITIONED: AtomicBool = AtomicBool::new(false);
pub static LAST_MPV_TRANSITION_MS: AtomicI64 = AtomicI64::new(0);
pub static MPV_STARTED_AT: AtomicI64 = AtomicI64::new(0);
pub static LAST_STARTUP_MS: AtomicI64 = AtomicI64::new(-1);
pub static LAST_TRANSITION_AT: AtomicI64 = AtomicI64::new(0);

fn log(gen: u64, msg: impl std::fmt::Display) {
    eprintln!("[T{}] {}", gen, msg);
}

// ── Sliding window ───────────────────────────────────────────────────

const WINDOW_SIZE: usize = 200;

struct SlidingWin {
    buf: VecDeque<u64>,
}

impl SlidingWin {
    fn new() -> Self {
        Self { buf: VecDeque::with_capacity(WINDOW_SIZE + 1) }
    }

    fn push(&mut self, val: u64) {
        if self.buf.len() >= WINDOW_SIZE {
            self.buf.pop_front();
        }
        self.buf.push_back(val);
    }

    fn percentile(&self, p: f64) -> u64 {
        let n = self.buf.len();
        if n == 0 {
            return 0;
        }
        let mut sorted: Vec<u64> = self.buf.iter().copied().collect();
        sorted.sort_unstable();
        let idx = ((p / 100.0) * (n as f64 - 1.0)).round() as usize;
        sorted[idx.min(n - 1)]
    }

    fn len(&self) -> usize { self.buf.len() }
    fn p95(&self) -> u64 { self.percentile(95.0) }
    fn p99(&self) -> u64 { self.percentile(99.0) }
    fn p50(&self) -> u64 { self.percentile(50.0) }
}

// ── Noise model ──────────────────────────────────────────────────────

struct NoiseModel {
    noise_drops: SlidingWin,
    idle_floor: SlidingWin,
    transition_rec: SlidingWin,
    trans_low_idle: SlidingWin,
    trans_low_count: u64,
    low_idle_count: u64,
    clean_samples: u64,
    warmup_ticks: u64,
    warmup_done: bool,
    consecutive_signal: u64,
    signal_triggered: bool,
    min_confirmations: u64,
    total_stops: u64,
    poll_ms: u64,
    tick_measurements: VecDeque<u64>,
    prev_idle: Option<u64>,
    method_tracking: Vec<(&'static str, f64, u64)>,
    block_until: Option<Instant>,
    trans_start: Option<Instant>,
    tli_contaminated: bool,
    tracked_exit: bool,
    last_summary: Instant,
    tick_count: u64,
    gen: u64,
}

impl NoiseModel {
    fn new(gen: u64) -> Self {
        Self {
            noise_drops: SlidingWin::new(),
            idle_floor: SlidingWin::new(),
            transition_rec: SlidingWin::new(),
            trans_low_idle: SlidingWin::new(),
            trans_low_count: 0,
            low_idle_count: 0,
            clean_samples: 0,
            warmup_ticks: 0,
            warmup_done: false,
            consecutive_signal: 0,
            signal_triggered: false,
            min_confirmations: 21, // bootstrap: tli p99=20 → +1: 21 confirms
            total_stops: 0,
            poll_ms: 50,
            tick_measurements: VecDeque::with_capacity(20),
            prev_idle: None,
            method_tracking: Vec::new(),
            block_until: None,
            trans_start: None,
            tli_contaminated: false,
            tracked_exit: false,
            last_summary: Instant::now(),
            tick_count: 0,
            gen,
        }
    }

    fn l(&self, msg: impl std::fmt::Display) {
        log(self.gen, msg);
    }

    fn track_method(&mut self, name: &'static str, latency_us: u64) {
        for entry in self.method_tracking.iter_mut() {
            if entry.0 == name {
                entry.2 += 1;
                let n = entry.2 as f64;
                entry.1 = entry.1 * ((n - 1.0) / n) + (latency_us as f64) / n;
                return;
            }
        }
        self.method_tracking.push((name, latency_us as f64, 1));
    }

    fn record_tick(&mut self, elapsed_us: u64) {
        self.tick_count += 1;
        self.tick_measurements.push_back(elapsed_us);
        if self.tick_measurements.len() > 20 {
            self.tick_measurements.pop_front();
        }

        if self.tick_count % 20 == 0 && self.tick_measurements.len() >= 10 {
            let avg_total = self.tick_measurements.iter().copied().sum::<u64>()
                / self.tick_measurements.len() as u64;
            let expected_sleep_us = self.poll_ms * 1000;
            let overhead = avg_total.saturating_sub(expected_sleep_us);
            let overhead_pct = if expected_sleep_us > 0 {
                overhead * 100 / expected_sleep_us
            } else {
                0
            };

            if overhead_pct > 30 && self.poll_ms < 200 {
                let old = self.poll_ms;
                self.poll_ms = (self.poll_ms + 10).min(200);
                self.l(format!("adapt_poll overhead={}% total={}us poll={}ms->{}ms",
                    overhead_pct, avg_total, old, self.poll_ms));
            } else if overhead_pct < 5 && self.poll_ms > 10 {
                let old = self.poll_ms;
                self.poll_ms = (self.poll_ms.saturating_sub(10)).max(5);
                self.l(format!("adapt_poll overhead={}% total={}us poll={}ms->{}ms",
                    overhead_pct, avg_total, old, self.poll_ms));
            }
        }
    }

    fn tighten_confirms(&mut self) {
        if self.trans_low_idle.len() < 3 { return }
        let tli_p99 = self.trans_low_idle.percentile(99.0);
        let noise_p99 = self.noise_p99() as u64;
        let from_tli = (tli_p99 + 1).max(2);
        let from_noise = noise_p99 / self.poll_ms.max(1);
        let target = from_tli.min(from_noise);
        if target < self.min_confirmations {
            let old = self.min_confirmations;
            self.min_confirmations = target;
            self.l(format!("tighten confirms={}->{} tli_p99={} noise_p99={}ms poll={}ms",
                old, target, tli_p99, noise_p99, self.poll_ms));
        }
    }

    fn push_tli(&mut self) {
        if self.tli_contaminated {
            self.l(format!("tli_skip contaminated={} count={}", self.tli_contaminated, self.trans_low_count));
            self.tli_contaminated = false;
            return;
        }
        let bw = self.block_window_ms();
        let max_possible = bw / self.poll_ms.max(1);
        let capped = self.trans_low_count.min(max_possible.max(1));
        self.trans_low_idle.push(capped);
        self.l(format!("tli_record={} capped={}/{} tli_p99={}",
            self.trans_low_count, capped, max_possible, self.trans_low_idle.percentile(99.0)));
        self.tighten_confirms();
    }

    fn feed_clean(&mut self, idle_ms: u64) {
        if let Some(prev) = self.prev_idle {
            if idle_ms < prev {
                self.noise_drops.push(prev - idle_ms);
            }
        }
        self.idle_floor.push(idle_ms);
        self.prev_idle = Some(idle_ms);
        self.clean_samples += 1;
        self.tighten_confirms();
    }

    fn block_window_ms(&self) -> u64 {
        let rec_p95 = self.transition_rec.percentile(95.0);
        if rec_p95 > 0 { rec_p95 } else { 1500 }
    }

    fn recovery_ms(&self) -> u64 {
        let bw = self.block_window_ms();
        let rec_p50 = self.transition_rec.percentile(50.0);
        bw + rec_p50
    }

    fn noise_p99(&self) -> u64 {
        if self.noise_drops.len() < 10 { 5000 }
        else { self.noise_drops.p99() }
    }

    fn noise_p95(&self) -> u64 {
        if self.noise_drops.len() < 10 { 2000 }
        else { self.noise_drops.p95() }
    }

    fn signal_strength(&self, idle_ms: u64, drop: u64) -> u64 {
        let p99 = self.noise_p99().max(1);
        let p95 = self.noise_p95().max(1);

        let mut score = 0u64;

        if drop > p99 * 4 { score += 100; }
        else if drop > p99 * 3 { score += 80; }
        else if drop > p99 * 2 { score += 50; }
        else if drop > p95 * 3 { score += 30; }
        else if drop > p95 * 2 { score += 10; }
        else if drop > p95 { score += 5; }

        if drop > p95 {
            if idle_ms < 500 { score += 30; }
            else if idle_ms < 2000 { score += 15; }
            if idle_ms == 0 { score += 10; }
        }

        score
    }

    fn evaluate(&mut self, idle_ms: u64, playing: bool, timeout: u64, autoplay: bool, videos_len: usize)
        -> Action
    {
        // ── Handle transition fire ───────────────────────────────────
        let mpv_transitioned = MPV_JUST_TRANSITIONED.swap(false, Ordering::SeqCst);
        if mpv_transitioned {
            // Skip duplicate events that arrive within an active block window
            if self.block_until.map(|b| Instant::now() < b).unwrap_or(false) {
                self.l(format!("dup_transition skip idle={}ms tli_count={}",
                    idle_ms, self.trans_low_count));
            } else {
                if self.trans_start.is_some() {
                    self.push_tli();
                }
                self.trans_start = Some(Instant::now());
                self.trans_low_count = 0;
                self.tli_contaminated = false;
                let bw = self.block_window_ms();
                self.block_until = Some(Instant::now() + std::time::Duration::from_millis(bw));
                // Don't reset signal detection if the user is clearly present.
                // A file transition doesn't mean the user vanished — they're still
                // interacting. Reset only when idle suggests the user walked away.
                if idle_ms < VERY_LOW {
                    self.l(format!("transition skip_signal_reset idle={}ms < VERY_LOW", idle_ms));
                } else {
                    self.signal_triggered = false;
                    self.consecutive_signal = 0;
                }
                self.low_idle_count = 0;
                self.l(format!("transition block={}ms idle={}ms p95={}ms p99={}ms rec_p95={}ms tli={}",
                    bw, idle_ms, self.noise_drops.p95(), self.noise_drops.p99(),
                    self.transition_rec.percentile(95.0), self.trans_low_count));
            }
        }

        let startup_ms = LAST_STARTUP_MS.swap(-1, Ordering::AcqRel);
        if startup_ms >= 0 {
            self.l(format!("mpv_startup={}ms", startup_ms));
        }

        let seconds = idle_ms / 1000;
        const VERY_LOW: u64 = 200;

        if !playing {
            if !self.tracked_exit && (idle_ms < VERY_LOW || self.signal_triggered) {
                self.tracked_exit = true;
                self.signal_triggered = false;
                self.consecutive_signal = 0;
                return Action::Stop;
            }
            if seconds >= timeout && autoplay && videos_len > 0 {
                return Action::AutoPlay;
            }
            return Action::None;
        }

        let rec_ms = self.recovery_ms();
        const TRIGGER_SCORE: u64 = 30;

        // ═══════════════════════════════════════════════════════════════
        // SIGNAL DETECTION
        // ═══════════════════════════════════════════════════════════════
        let drop = self.prev_idle.map(|p| p.saturating_sub(idle_ms)).unwrap_or(0);
        self.prev_idle = Some(idle_ms);
        let score = self.signal_strength(idle_ms, drop);

        // ── Measure transition recovery + low-idle duration ──────────
        if let Some(start) = self.trans_start {
            if idle_ms < VERY_LOW {
                self.trans_low_count += 1;
            }
            if idle_ms > VERY_LOW * 5 / 2 {
                let elapsed = start.elapsed().as_millis() as u64;
                self.transition_rec.push(elapsed);
                self.push_tli();
                self.l(format!("transition_recovery={}ms rec_p95={}ms idle={}ms",
                    elapsed, self.transition_rec.percentile(95.0), idle_ms));
                self.trans_start = None;
            }
        }

        // ── Block window expired — force trigger if user still present ──
        let blocked = self.block_until.map(|b| Instant::now() < b).unwrap_or(false);
        if !blocked && self.block_until.is_some() {
            self.block_until = None;
            // Flush trans_low_idle if recovery measurement never fired (block timeout)
            if self.trans_start.is_some() {
                self.push_tli();
                self.trans_start = None;
            }
            if idle_ms < VERY_LOW {
                self.tli_contaminated = true;
                self.signal_triggered = true;
                self.consecutive_signal = 1;
                self.l(format!("FORCE_TRIGGER after block idle={}ms conf=1/{}",
                    idle_ms, self.min_confirmations));
            }
        }

        // ── TRIGGER — drop-based, only when NOT in block window ──────
        if !self.signal_triggered && !blocked {
            if score >= TRIGGER_SCORE {
                self.signal_triggered = true;
                self.consecutive_signal = 1;
                self.l(format!("TRIGGER idle={}ms drop={}ms score={} conf=1/{} p95={}ms p99={}ms rec={}ms",
                    idle_ms, drop, score, self.min_confirmations,
                    self.noise_drops.p95(), self.noise_drops.p99(), rec_ms));
            }
        }

        // ── TRIGGER — duration-based: idle stays low for extended period ──
        if !self.signal_triggered && !blocked && self.warmup_done {
            if idle_ms < 500 {
                self.low_idle_count += 1;
                let needed = (self.min_confirmations * 2).max(4);
                if self.low_idle_count >= needed {
                    self.signal_triggered = true;
                    self.consecutive_signal = 1;
                    self.l(format!("LOW_IDLE_TRIGGER idle={}ms count={}/{} confs={}",
                        idle_ms, self.low_idle_count, needed, self.min_confirmations));
                }
            } else {
                self.low_idle_count = 0;
            }
        }

        // ── CONFIRM — only count ticks where idle is VERY_LOW ────
        // Stalls when idle is between VERY_LOW and rec_ms; aborts when idle >= rec_ms
        if self.signal_triggered {
            if idle_ms < rec_ms {
                if idle_ms < VERY_LOW {
                    self.consecutive_signal += 1;
                    if self.consecutive_signal >= self.min_confirmations {
                        self.total_stops += 1;
                        let p95 = self.noise_drops.p95();
                        let p99 = self.noise_drops.p99();
                        self.l(format!("STOP total={} idle={}ms drop={}ms score={} conf={}/{} p95={}ms p99={}ms rec={}ms poll={}ms clean={}",
                            self.total_stops, idle_ms, drop, score,
                            self.consecutive_signal, self.min_confirmations,
                            p95, p99, rec_ms, self.poll_ms, self.clean_samples));
                        self.signal_triggered = false;
                        self.consecutive_signal = 0;
                        self.tracked_exit = true;
                        return Action::Stop;
                    }
                }
                self.l(format!("CONFIRM tick={} idle={}ms conf={}/{} rec={}ms",
                    self.tick_count, idle_ms,
                    self.consecutive_signal, self.min_confirmations, rec_ms));
            } else {
                self.l(format!("ABORT idle={}ms >= rec={}ms conf={}/{}",
                    idle_ms, rec_ms, self.consecutive_signal, self.min_confirmations));
                self.signal_triggered = false;
                self.consecutive_signal = 0;
            }
        }

        // ═══════════════════════════════════════════════════════════════
        // FEEDING
        // ═══════════════════════════════════════════════════════════════

        if !self.warmup_done {
            self.warmup_ticks += 1;

            if seconds as u64 >= timeout / 2 && self.block_until.map(|b| Instant::now() >= b).unwrap_or(true) {
                self.feed_clean(idle_ms);
            }

            if self.warmup_ticks >= WINDOW_SIZE as u64 {
                self.warmup_done = true;
                self.l(format!("warmup_done ticks={} clean={} p95_drop={}ms p99_drop={}ms p50_idle={}ms confirms={} rec_p95={}ms tli_p99={} poll={}ms",
                    self.warmup_ticks, self.clean_samples,
                    self.noise_drops.p95(), self.noise_drops.p99(),
                    self.idle_floor.p50(), self.min_confirmations,
                    self.transition_rec.percentile(95.0),
                    self.trans_low_idle.percentile(99.0), self.poll_ms));
            }
            return Action::None;
        }

        // Active: feed clean when user is away, not blocked, not triggered
        if seconds as u64 >= timeout / 2 && !blocked && !self.signal_triggered {
            self.feed_clean(idle_ms);
        }

        // Periodic summary
        if self.tick_count % 100 == 0 && self.last_summary.elapsed().as_secs() >= 2 {
            let method_info: Vec<String> = self.method_tracking.iter()
                .map(|(n, avg, c)| format!("{}={:.0}us/{}c", n, avg, c))
                .collect();
            self.l(format!("trace tick={} poll={}ms phase={} idle={}ms drop={}ms score={} triggered={} conf={}/{} p95_drop={}ms p99_drop={}ms p50_idle={}ms rec_p95={}ms tli_p99={} clean={} stops={} methods=[{}]",
                self.tick_count, self.poll_ms,
                if self.warmup_done { "active" } else { "warmup" },
                idle_ms, drop, score,
                self.signal_triggered,
                self.consecutive_signal, self.min_confirmations,
                self.noise_drops.p95(), self.noise_drops.p99(), self.idle_floor.p50(),
                self.transition_rec.percentile(95.0),
                self.trans_low_idle.percentile(99.0),
                self.clean_samples, self.total_stops,
                method_info.join(" ")));
            self.last_summary = Instant::now();
        }

        Action::None
    }
}

enum Action {
    None,
    Stop,
    AutoPlay,
}

// ── Idle monitor ─────────────────────────────────────────────────────

#[tauri::command]
fn start_idle_monitor(app: AppHandle) {
    let gen = MONITOR_GEN.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    log(gen, format!("start_idle_monitor gen={}", gen));
    IDLE_MONITOR_ACTIVE.store(true, Ordering::SeqCst);
    let app_clone = app.clone();
    std::thread::spawn(move || {
        idle_monitor_loop(app_clone, gen);
    });
}

#[tauri::command]
fn stop_idle_monitor() {
    let gen = MONITOR_GEN.load(Ordering::Relaxed);
    log(gen, "stop_idle_monitor");
    IDLE_MONITOR_ACTIVE.store(false, Ordering::SeqCst);
}

#[tauri::command]
fn is_idle_monitor_active() -> bool {
    IDLE_MONITOR_ACTIVE.load(Ordering::SeqCst)
}

#[tauri::command]
fn is_playing() -> bool {
    IS_PLAYING.load(Ordering::SeqCst)
}

#[tauri::command]
fn toggle_idle_monitor(app: AppHandle) {
    let state = app.state::<ConfigState>();
    let mut config = state.config.lock().unwrap();
    config.global.idle_enabled = !config.global.idle_enabled;
    let enabled = config.global.idle_enabled;
    let _ = state.save(&config);
    drop(config);

    if enabled {
        start_idle_monitor(app);
    } else {
        stop_idle_monitor();
        if IS_PLAYING.load(std::sync::atomic::Ordering::SeqCst) {
            IS_PLAYING.store(false, std::sync::atomic::Ordering::SeqCst);
            stop_playback(&app);
        }
    }
}

fn idle_monitor_loop(app: AppHandle, gen: u64) {
    let mut model = NoiseModel::new(gen);
    let mut ai: u64 = 0;

    model.l(format!("monitor_started poll={}ms confirms={}",
        model.poll_ms, model.min_confirmations));

    while IDLE_MONITOR_ACTIVE.load(Ordering::SeqCst)
        && MONITOR_GEN.load(Ordering::SeqCst) == gen
    {
        let tick_start = Instant::now();

        std::thread::sleep(std::time::Duration::from_millis(model.poll_ms));

        if MONITOR_GEN.load(Ordering::SeqCst) != gen {
            model.l("stale_gen — exiting");
            break;
        }

        let state = app.state::<ConfigState>();
        let config = state.config.lock().unwrap();
        let timeout = config.global.idle_timeout_seconds;
        let enabled = config.global.idle_enabled;
        let autoplay_enabled = config.global.autoplay_on_idle;
        let videos = config.videos.clone();
        let gp = config.global.default_params.clone();
        let lpc = config.global.last_played_id.clone();
        let playing = IS_PLAYING.load(Ordering::SeqCst);
        drop(config);

        if !enabled || videos.is_empty() {
            ai = 0;
            model.prev_idle = None;
            continue;
        }

        let idle_result = match idle::IdleDetector::idle_ms() {
            Ok(r) => r,
            Err(e) => {
                model.l(format!("detect_error={}", e));
                ai = 0;
                continue;
            }
        };
        let idle_ms = idle_result.idle_ms;

        model.record_tick(tick_start.elapsed().as_micros() as u64);

        for s in &idle_result.samples {
            model.track_method(s.method, s.latency_us);
        }

        let seconds = idle_ms / 1000;
        let rem = timeout.saturating_sub(seconds.min(timeout));
        let _ = app.emit("idle-status", serde_json::json!({
            "remaining": rem,
            "idle_ms": idle_ms,
            "total_latency_us": idle_result.total_latency_us,
            "poll_ms": model.poll_ms,
            "phase": if model.warmup_done { "active" } else { "warmup" },
            "confirms": model.min_confirmations,
            "triggered": model.signal_triggered,
            "samples": idle_result.samples.iter().map(|s| serde_json::json!({
                "method": s.method,
                "idle_ms": s.idle_ms,
                "latency_us": s.latency_us,
            })).collect::<Vec<_>>(),
        }));

        match model.evaluate(idle_ms, playing, timeout, autoplay_enabled, videos.len()) {
            Action::Stop => {
                IS_PLAYING.store(false, Ordering::SeqCst);
                let _ = app.emit("playback-stopped", ());
                stop_playback(&app);
                ai = 0;
            }
            Action::AutoPlay => {
                if autoplay_enabled {
                    if IS_PLAYING.load(Ordering::SeqCst) {
                        ai = 0;
                    } else {
                        ai += 1;
                        model.l(format!("autoplay accum ai={}/2", ai));
                        if ai >= 2 {
                            let started_now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as i64;
                            MPV_STARTED_AT.store(started_now, Ordering::Release);
                            model.l(format!("autoplay FIRE videos={} rotate={:?}",
                                videos.len(), lpc));
                            IS_PLAYING.store(true, Ordering::SeqCst);
                            let _ = app.emit("playback-started", ());
                            let ac2 = app.clone();
                            let v2 = videos.clone();
                            let p2 = gp.clone();
                            let rotate = lpc.and_then(|id| v2.iter().position(|v| v.id == id));
                            std::thread::spawn(move || {
                                if let Err(e) = start_playback(&ac2, &v2, &p2, rotate) {
                                    log(0, format!("autoplay_error={}", e));
                                }
                            });
                            ai = 0;
                        }
                    }
                } else {
                    ai = 0;
                }
            }
            Action::None => {
                ai = 0;
            }
        }

        let elapsed_us = tick_start.elapsed().as_micros() as u64;
        if elapsed_us > model.poll_ms * 1500 {
            model.l(format!("slow_tick elapsed={}ms poll={}ms ovh={}ms",
                elapsed_us / 1000, model.poll_ms,
                elapsed_us.saturating_sub(model.poll_ms * 1000) / 1000));
        }
    }

    if MONITOR_GEN.load(Ordering::SeqCst) != gen {
        model.l("superseded — exiting");
    }

    if IS_PLAYING.load(Ordering::SeqCst) {
        IS_PLAYING.store(false, Ordering::SeqCst);
        stop_playback(&app);
    }
    model.l(format!("monitor_stopped ticks={} stops={} clean={} tli_p99={}",
        model.tick_count, model.total_stops, model.clean_samples,
        model.trans_low_idle.percentile(99.0)));
}

// ── Playback commands ─────────────────────────────────────────────────

#[tauri::command]
fn manual_play(app: AppHandle, state: tauri::State<ConfigState>,
    selected_ids: Vec<String>) -> Result<(), String> {
    let _t = std::time::Instant::now();
    let config = state.config.lock().unwrap();
    let videos = config.videos.clone();
    let global = config.global.default_params.clone();
    let last_played = config.global.last_played_id.clone();
    drop(config);

    if videos.is_empty() {
        return Err("No videos in loop".into());
    }

    let rotate_to: Option<usize> = if selected_ids.is_empty() {
        last_played.and_then(|id| videos.iter().position(|v| v.id == id))
    } else {
        let found: Vec<usize> = videos.iter()
            .enumerate()
            .filter(|(_, v)| selected_ids.contains(&v.id))
            .map(|(i, _)| i)
            .collect();
        found.into_iter().min()
    };

    let started_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    MPV_STARTED_AT.store(started_now, Ordering::Release);
    IS_PLAYING.store(true, Ordering::SeqCst);
    start_playback(&app, &videos, &global, rotate_to).map_err(|e| e.to_string())?;
    let _ = app.emit("playback-started", ());
    log(0, format!("manual_play {}us", _t.elapsed().as_micros()));
    Ok(())
}

#[tauri::command]
fn manual_stop(app: AppHandle) -> Result<(), String> {
    let _t = std::time::Instant::now();
    IS_PLAYING.store(false, Ordering::SeqCst);
    stop_playback(&app);
    let _ = app.emit("playback-stopped", ());
    log(0, format!("manual_stop {}us", _t.elapsed().as_micros()));
    Ok(())
}

fn start_playback(app: &AppHandle, videos: &[config::VideoItem], global: &config::VideoParams,
    rotate_to: Option<usize>) -> Result<(), player::PlayerError> {
    let _t = std::time::Instant::now();
    let player = app.state::<MpvPlayer>();
    let r = player.start_with_monitor(videos, global, rotate_to, app.clone());
    log(0, format!("start_playback {}us", _t.elapsed().as_micros()));
    r
}

fn stop_playback(app: &AppHandle) {
    let _t = std::time::Instant::now();
    LAST_MPV_TRANSITION_MS.store(0, Ordering::SeqCst);
    LAST_TRANSITION_AT.store(0, Ordering::Release);
    MPV_STARTED_AT.store(0, Ordering::Release);
    if let Some(last_id) = LAST_PLAYED_UUID.lock().unwrap().take() {
        let state = app.state::<ConfigState>();
        let mut config = state.config.lock().unwrap();
        config.global.last_played_id = Some(last_id);
        let _ = state.save(&config);
    }
    let player = app.state::<MpvPlayer>();
    player.stop();
    log(0, format!("stop_playback {}us", _t.elapsed().as_micros()));
}

// ── Tauri entry point ─────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .manage(ConfigState::new().expect("Failed to initialize config"))
        .manage(MpvPlayer::new())
        .invoke_handler(tauri::generate_handler![
            config::get_config,
            config::update_global_settings,
            config::add_videos,
            config::remove_video,
            config::reorder_videos,
            config::update_video_params,
            start_idle_monitor,
            stop_idle_monitor,
            is_idle_monitor_active,
            is_playing,
            toggle_idle_monitor,
            manual_play,
            manual_stop,
            thumbnail::get_thumbnail_path,
            thumbnail::get_thumbnail_base64,
        ])
        .setup(|app| {
            let open_manager = MenuItemBuilder::new("Open Manager")
                .id("open_manager")
                .build(app)?;
            let toggle_idle = MenuItemBuilder::new("Toggle Idle Mode")
                .id("toggle_idle")
                .build(app)?;
            let quit = MenuItemBuilder::new("Quit")
                .id("quit")
                .build(app)?;

            let menu = MenuBuilder::new(app)
                .item(&open_manager)
                .item(&toggle_idle)
                .separator()
                .item(&quit)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("Daydream — Idle Video Looper")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open_manager" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "toggle_idle" => {
                        let state = app.state::<ConfigState>();
                        let mut config = state.config.lock().unwrap();
                        config.global.idle_enabled = !config.global.idle_enabled;
                        let enabled = config.global.idle_enabled;
                        let _ = state.save(&config);
                        if enabled {
                            start_idle_monitor(app.clone());
                        } else {
                            stop_idle_monitor();
                            if IS_PLAYING.load(std::sync::atomic::Ordering::SeqCst) {
                                IS_PLAYING.store(false, std::sync::atomic::Ordering::SeqCst);
                                stop_playback(app);
                            }
                        }
                    }
                    "quit" => {
                        stop_idle_monitor();
                        stop_playback(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            let state = app.state::<ConfigState>();
            let config = state.config.lock().unwrap();
            let idle_enabled = config.global.idle_enabled;
            drop(config);

            if idle_enabled {
                start_idle_monitor(app.handle().clone());
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Daydream");
}
