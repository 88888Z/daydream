mod config;
mod idle;
mod player;
mod thumbnail;

use config::ConfigState;
use player::MpvPlayer;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tauri::{
    AppHandle, Emitter, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

static IDLE_MONITOR_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static IS_PLAYING: AtomicBool = AtomicBool::new(false);
pub static LAST_PLAYED_ENTRY_MS: AtomicI64 = AtomicI64::new(-1);
pub static MPV_JUST_TRANSITIONED: AtomicBool = AtomicBool::new(false);
pub static LAST_MPV_TRANSITION_MS: AtomicI64 = AtomicI64::new(0);
pub static MPV_STARTED_AT: AtomicI64 = AtomicI64::new(0);
pub static LAST_STARTUP_MS: AtomicI64 = AtomicI64::new(-1);
pub static LAST_TRANSITION_AT: AtomicI64 = AtomicI64::new(0);

#[tauri::command]
fn start_idle_monitor(app: AppHandle) {
    IDLE_MONITOR_ACTIVE.store(true, Ordering::SeqCst);
    let app_clone = app.clone();
    std::thread::spawn(move || {
        idle_monitor_loop(app_clone);
    });
}

#[tauri::command]
fn stop_idle_monitor() {
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
fn manual_play(app: AppHandle, state: tauri::State<ConfigState>,
    selected_ids: Vec<String>) -> Result<(), String> {
    let _t = std::time::Instant::now();
    let config = state.config.lock().unwrap();
    let videos = config.videos.clone();
    let global = config.global.default_params.clone();
    let last_played = config.global.last_played_entry;
    drop(config);

    if videos.is_empty() {
        return Err("No videos in loop".into());
    }

    let rotate_to = if selected_ids.is_empty() {
        last_played
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
    eprintln!("[TIMING] manual_play {}us", _t.elapsed().as_micros());
    Ok(())
}

#[tauri::command]
fn manual_stop(app: AppHandle) -> Result<(), String> {
    let _t = std::time::Instant::now();
    IS_PLAYING.store(false, Ordering::SeqCst);
    stop_playback(&app);
    let _ = app.emit("playback-stopped", ());
    eprintln!("[TIMING] manual_stop {}us", _t.elapsed().as_micros());
    Ok(())
}

use std::collections::VecDeque;

fn avg<I: IntoIterator<Item = u64>>(vals: I) -> u64 {
    let mut s = 0u64; let mut n = 0u64;
    for v in vals { s += v; n += 1; }
    if n == 0 { 0 } else { s / n }
}

struct Cal {
    poll_ms: u64, drop_thresh: u64,
    nz_target: u64, jitter_window_ms: u64,
    max_jitter_drop: u64,
    jitter_drops: VecDeque<u64>,
    jitter_rec_ms: VecDeque<u64>,
    detect_avg_us: u64, detect_n: u64,
    tick_avg_us: u64, tick_n: u64,
    last_stop: Option<std::time::Instant>,
    jitter_start: Option<std::time::Instant>,
    cycles: u64,
    startup_ms: VecDeque<u64>,
    startup_window_ms: u64,
}

impl Cal {
    fn new() -> Self {
        Self {
            poll_ms: 200, drop_thresh: 1500, nz_target: 4, jitter_window_ms: 2000,
            max_jitter_drop: 0,
            jitter_drops: VecDeque::new(), jitter_rec_ms: VecDeque::new(),
            detect_avg_us: 0, detect_n: 0, tick_avg_us: 0, tick_n: 0,
            last_stop: None, jitter_start: None, cycles: 0,
            startup_ms: VecDeque::new(), startup_window_ms: 0,
        }
    }

    fn env_factor(&self) -> u64 {
        let n = self.jitter_drops.len();
        if n < 3 { return 120; }
        let m = avg(self.jitter_drops.iter().copied());
        let v = avg(self.jitter_drops.iter().map(|&x| x.abs_diff(m).pow(2)));
        let sd = (v as f64).sqrt() as u64;
        if m == 0 { 120 } else { ((m + sd * 3) * 100 / m).max(110).min(300) }
    }

    fn calibrate_nz(&mut self) { self.nz_target = (2000u64 / self.poll_ms).max(3); }

    fn record_startup(&mut self, ms: u64) {
        self.startup_ms.push_back(ms);
        if self.startup_ms.len() > 10 { self.startup_ms.pop_front(); }
        self.calibrate_startup_window();
        eprintln!("[CAL] startup={}ms n={} window={}ms", ms, self.startup_ms.len(), self.startup_window_ms);
    }

    fn calibrate_startup_window(&mut self) {
        let n = self.startup_ms.len();
        if n == 0 {
            self.startup_window_ms = 0;
            return;
        }
        let mean = avg(self.startup_ms.iter().copied());
        if n == 1 {
            self.startup_window_ms = mean + 3000;
            eprintln!("[CAL] startup_win={}ms (single sample {}ms + 3000)", self.startup_window_ms, mean);
            return;
        }
        let variance = avg(self.startup_ms.iter().map(|&x| x.abs_diff(mean).pow(2)));
        let sd = (variance as f64).sqrt() as u64;
        let margin = (mean as f64 * 0.15) as u64; // 15% safety margin
        let new = mean + sd * 3 + margin;
        if new.abs_diff(self.startup_window_ms) > 100 || n <= 2 {
            eprintln!("[CAL] startup_win mean={} sd={} margin={} {}→{}ms (n={})",
                mean, sd, margin, self.startup_window_ms, new, n);
            self.startup_window_ms = new;
        }
    }

    fn within_startup(&self) -> bool {
        let started = MPV_STARTED_AT.load(std::sync::atomic::Ordering::Acquire);
        if started == 0 { return false; }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let elapsed = now - started;
        let win = if self.startup_window_ms > 0 { self.startup_window_ms } else { 5000 };
        let inside = elapsed >= 0 && elapsed < win as i64;
        if !inside && self.startup_window_ms == 0 && elapsed > 0 {
            eprintln!("[CAL] startup_win first-guess {}ms (uncalibrated) elapsed={}ms", win, elapsed);
        }
        if inside && elapsed % 1000 < 20 { eprintln!("[CAL] within_startup elapsed={}ms win={}ms", elapsed, win); }
        inside
    }

    fn within_trans_grace(&self) -> bool {
        let ts = LAST_TRANSITION_AT.load(std::sync::atomic::Ordering::Acquire);
        if ts == 0 { return false; }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let elapsed = now - ts;
        let grace_ms = self.jitter_window_ms.max(5000);
        let inside = elapsed >= 0 && elapsed < grace_ms as i64;
        if inside && elapsed % 2000 < 20 {
            eprintln!("[CAL] trans_grace {}ms win={}ms", elapsed, grace_ms);
        }
        inside
    }

    fn calibrate_win(&mut self) {
        let r = avg(self.jitter_rec_ms.iter().copied());
        self.jitter_window_ms = if r > 0 { r + 1000 } else { 2000 };
    }

    fn record_overhead(&mut self, us: u64) {
        self.tick_n += 1;
        self.tick_avg_us = (self.tick_avg_us * (self.tick_n - 1) + us) / self.tick_n;
        if self.tick_n >= 5 {
            let budget = self.poll_ms * 80;
            if self.tick_avg_us > budget && self.poll_ms < 500 {
                let old = self.poll_ms;
                self.poll_ms = (self.poll_ms + 50).min(500);
                eprintln!("[CAL] ovh={}us > budget={}us poll {}→{}ms nz={}", self.tick_avg_us, budget, old, self.poll_ms, self.nz_target);
                self.calibrate_nz();
            } else if self.tick_avg_us < budget / 4 && self.poll_ms > 100 && self.tick_n > 20 {
                let old = self.poll_ms;
                self.poll_ms = (self.poll_ms - 50).max(100);
                eprintln!("[CAL] ovh={}us < budget/4 poll {}→{}ms nz={}", self.tick_avg_us, old, self.poll_ms, self.nz_target);
                self.calibrate_nz();
            }
        }
    }

    fn record_detect(&mut self, us: u64) {
        self.detect_n += 1;
        self.detect_avg_us = (self.detect_avg_us * (self.detect_n - 1) + us) / self.detect_n;
        if self.detect_n == 10 { eprintln!("[CAL] detect avg={}us n={}", self.detect_avg_us, self.detect_n); }
    }

    fn record_jitter(&mut self, drop: u64, now: std::time::Instant) {
        if drop > self.max_jitter_drop { self.max_jitter_drop = drop; }
        self.jitter_drops.push_back(drop);
        if self.jitter_drops.len() > 10 { self.jitter_drops.pop_front(); }
        self.jitter_start = Some(now);
        let f = self.env_factor();
        let nd = (drop * f / 100).max(self.drop_thresh);
        if nd != self.drop_thresh || drop == self.max_jitter_drop {
            let old = self.drop_thresh;
            self.drop_thresh = nd;
            if old != nd { eprintln!("[CAL] jit drop={} env={}% dt={}→{}ms", drop, f, old, self.drop_thresh); }
        }
    }

    fn record_recovery(&mut self) {
        if let Some(start) = self.jitter_start {
            let ms = start.elapsed().as_millis() as u64;
            self.jitter_rec_ms.push_back(ms);
            if self.jitter_rec_ms.len() > 5 { self.jitter_rec_ms.pop_front(); }
            self.calibrate_win();
            eprintln!("[CAL] rec={}ms win={}ms n={}", ms, self.jitter_window_ms, self.jitter_rec_ms.len());
            self.jitter_start = None;
        }
    }

    fn record_stop(&mut self, timeout: u64) {
        let now = std::time::Instant::now();
        let gap = self.last_stop.map(|t| t.elapsed().as_secs_f64()).unwrap_or(999.0);
        if gap < 30.0 {
            self.cycles += 1;
            let nz = (self.nz_target + self.cycles * timeout / 2).min(timeout * 2);
            if nz != self.nz_target {
                eprintln!("[CAL] cycle#{} gap={}s nz={}→{}", self.cycles, gap as u64, self.nz_target, nz);
                self.nz_target = nz;
            }
        } else {
            if self.cycles > 0 { eprintln!("[CAL] calm {}s after {} cycles nz={}→{}", gap as u64, self.cycles, self.nz_target, self.poll_ms); }
            self.calibrate_nz();
            self.cycles = 0;
        }
        self.last_stop = Some(now);
    }
}

fn idle_monitor_loop(app: AppHandle) {
    let mut prev: Option<u64> = None;
    let mut ai: u64 = 0;
    let mut nz: u64 = 0;
    let mut steady: bool = false;
    let mut ac: bool = false;
    let mut cd: u64 = 0;
    let mut jitter_until: Option<std::time::Instant> = None;
    let mut post_jitter: bool = false;
    let mut cal = Cal::new();
    let mut hist: [u64; 10] = [0; 10];
    let mut hp: usize = 0;

    while IDLE_MONITOR_ACTIVE.load(Ordering::SeqCst) {
        let _ts = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(cal.poll_ms));
        let _after_sleep = std::time::Instant::now();

        let state = app.state::<ConfigState>();
        let config = state.config.lock().unwrap();
        let timeout = config.global.idle_timeout_seconds;
        let enabled = config.global.idle_enabled;
        let autoplay = config.global.autoplay_on_idle;
        let videos = config.videos.clone();
        let gp = config.global.default_params.clone();
        let lpc = config.global.last_played_entry;
            let playing = IS_PLAYING.load(Ordering::SeqCst);
            // Feed any new mpv startup measurement into calibration
            let startup_ms = LAST_STARTUP_MS.swap(-1, Ordering::AcqRel);
            if startup_ms >= 0 { cal.record_startup(startup_ms as u64); }
            drop(config);

            if !enabled || videos.is_empty() {
            prev = None; ai = 0; nz = 0; steady = false; ac = false; hp = 0; cd = 0;
            continue;
        }

        let t0 = std::time::Instant::now();
        let idle_ms = match idle::IdleDetector::idle_ms() {
            Ok(ms) => { cal.record_detect(t0.elapsed().as_micros() as u64); ms },
            Err(e) => { eprintln!("[idle] detect error {e}"); prev = None; ai = 0; nz = 0; steady = false; ac = false; hp = 0; cd = 0; continue }
        };

        hist[hp] = idle_ms; hp = (hp + 1) % hist.len();
        let s = idle_ms / 1000;
        let p = prev;
        let drop = p.map(|x| x.saturating_sub(idle_ms)).unwrap_or(0);
        let det = drop > cal.drop_thresh;
        let mpv = MPV_JUST_TRANSITIONED.swap(false, Ordering::SeqCst);

        if !steady && s >= timeout { steady = true; eprintln!("[idle] steady s={} >= timeout={}", s, timeout); }

        let rem = timeout.saturating_sub(s.min(timeout));
        let _ = app.emit("idle-status", serde_json::json!({ "remaining": rem }));

        // record jitter for calibration (big drops without mpv, but not user — just observing)
        if det && !mpv && playing && !cal.within_startup() {
            let h: Vec<String> = hist.iter().enumerate().map(|(i, &v)| format!("{}:{}", i, v)).collect();
            cd += 1;
            let now = std::time::Instant::now();
            // record jitter sample
            if cal.detect_n > 5 { cal.record_jitter(drop, now); }
            // suppress NZ if within measured envelope
            let factor = cal.env_factor();
            if cal.max_jitter_drop > 0 && drop <= cal.drop_thresh {
                let win_ms = cal.jitter_window_ms;
                jitter_until = Some(now + std::time::Duration::from_millis(win_ms));
                eprintln!("[CAL] jit drop={} env={}% win={}ms nz={}", drop, factor, win_ms, cal.nz_target);
            }
            eprintln!("[idle] DETECT#{} drop={}ms hist=[{}] p={:?} c={}", cd, drop, h.join(","), p, idle_ms);
        } else { cd = 0; }

        if det && mpv { nz = 0; ac = false; prev = Some(idle_ms); ai = 0; continue; }
        if playing && !det && s >= 1 {
            if s == 1 { cal.record_recovery(); }
            nz = 0; ac = true; prev = Some(idle_ms); ai = 0; continue;
        }

        if playing && s < 1 && !mpv {
            if !steady { prev = Some(idle_ms); ai = 0; continue; }
            if ac { ac = false; prev = Some(idle_ms); ai = 0; continue; }
            if cal.within_startup() { prev = Some(idle_ms); ai = 0; continue; }
            if cal.within_trans_grace() { prev = Some(idle_ms); ai = 0; continue; }
            if let Some(until) = jitter_until {
                if std::time::Instant::now() < until {
                    prev = Some(idle_ms); ai = 0; continue;
                }
                jitter_until = None;
                post_jitter = true;
                eprintln!("[CAL] jitter_win_expired fast_nz={}", (1000 / cal.poll_ms).max(2));
            }
            let effective_nz = if post_jitter { (1000 / cal.poll_ms).max(2) } else { cal.nz_target };
            nz += 1; prev = Some(idle_ms); ai = 0;
            if nz >= effective_nz {
                cal.record_stop(timeout);
                eprintln!("[idle] NZ-STOP idle={}ms nz={}/{} nz_target={} startup_win={}ms poll={}ms", idle_ms, nz, effective_nz, cal.nz_target, cal.startup_window_ms, cal.poll_ms);
                nz = 0; ac = false; post_jitter = false;
                IS_PLAYING.store(false, Ordering::SeqCst);
                let _ = app.emit("playback-stopped", ());
                stop_playback(&app);
            }
            continue;
        }

        nz = 0; jitter_until = None; post_jitter = false; prev = Some(idle_ms);

        if s >= timeout {
            ac = false;
            if ai == 0 && !playing { eprintln!("[idle] climbing r={}s idle={}ms", rem, idle_ms); }
            ai += 1;
            if ai >= 2 && autoplay && !playing {
                let started_now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                MPV_STARTED_AT.store(started_now, Ordering::Release);
                eprintln!("[idle] AUTOPLAY videos={} rotate={:?} started_at={}", videos.len(), lpc, started_now);
                IS_PLAYING.store(true, Ordering::SeqCst);
                let _ = app.emit("playback-started", ());
                let ac2 = app.clone(); let v2 = videos.clone(); let p2 = gp.clone();
                std::thread::spawn(move || { if let Err(e) = start_playback(&ac2, &v2, &p2, lpc) { eprintln!("[idle] autoplay {e}"); } });
            }
        } else { ai = 0; }

        let tus = _ts.elapsed().as_micros() as u64;
        let overhead_us = tus.saturating_sub(cal.poll_ms * 1000);
        cal.record_overhead(overhead_us);
        if overhead_us > cal.poll_ms * 500 {
            let jw = jitter_until.map(|u| u.saturating_duration_since(std::time::Instant::now()).as_secs()).unwrap_or(0);
            eprintln!("[TIMING] tick {}ms p={}ms ovh={}ms jw={}s nz={} dt={} cyc={} win={}ms rec={}",
                tus / 1000, cal.poll_ms, overhead_us / 1000,
                jw, cal.nz_target, cal.drop_thresh, cal.cycles,
                cal.jitter_window_ms, avg(cal.jitter_rec_ms.iter().copied()));
        }
    }

    if IS_PLAYING.load(Ordering::SeqCst) { IS_PLAYING.store(false, Ordering::SeqCst); stop_playback(&app); }
}

fn start_playback(app: &AppHandle, videos: &[config::VideoItem], global: &config::VideoParams,
    rotate_to: Option<usize>) -> Result<(), player::PlayerError> {
    let _t = std::time::Instant::now();
    let player = app.state::<MpvPlayer>();
    let r = player.start_with_monitor(videos, global, rotate_to, app.clone());
    eprintln!("[TIMING] start_playback {}us", _t.elapsed().as_micros());
    r
}

fn stop_playback(app: &AppHandle) {
    let _t = std::time::Instant::now();
    LAST_MPV_TRANSITION_MS.store(0, Ordering::SeqCst);
    LAST_TRANSITION_AT.store(0, Ordering::Release);
    MPV_STARTED_AT.store(0, Ordering::Release);
    let entry = LAST_PLAYED_ENTRY_MS.load(Ordering::SeqCst);
    if entry >= 0 {
        let state = app.state::<ConfigState>();
        let mut config = state.config.lock().unwrap();
        config.global.last_played_entry = Some(entry as usize);
        eprintln!("[daydream] stop_playback: saving last_played_entry={:?} to config.json", config.global.last_played_entry);
        let _ = state.save(&config);
    }
    let player = app.state::<MpvPlayer>();
    player.stop();
    eprintln!("[TIMING] stop_playback {}us", _t.elapsed().as_micros());
}

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
