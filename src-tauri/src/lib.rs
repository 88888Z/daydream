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

#[derive(Debug, Clone)]
struct Cal {
    poll_ms: u64, drop_thresh: u64,
    nz_base: u64, nz_curr: u64, nz_max: u64,
    max_jitter_drop: u64,
    detect_avg_us: u64, detect_samples: u64, tick_avg_us: u64, tick_samples: u64,
    last_stop_at: Option<std::time::Instant>, cycle_speed: u64, cycles: u64,
}

impl Cal {
    fn new() -> Self {
        Self {
            poll_ms: 200, drop_thresh: 1500,
            nz_base: 4, nz_curr: 4, nz_max: 4,
            max_jitter_drop: 0,
            detect_avg_us: 0, detect_samples: 0, tick_avg_us: 0, tick_samples: 0,
            last_stop_at: None, cycle_speed: 0, cycles: 0,
        }
    }

    fn record_overhead(&mut self, overhead_us: u64) {
        if self.tick_samples == 0 { self.tick_avg_us = overhead_us; }
        else { self.tick_avg_us = (self.tick_avg_us * self.tick_samples + overhead_us) / (self.tick_samples + 1); }
        self.tick_samples += 1;
        if self.tick_samples >= 5 {
            // overhead budget: 10% of poll time
            let target_overhead = self.poll_ms * 100;
            if self.tick_avg_us > target_overhead && self.poll_ms < 500 {
                let old = self.poll_ms;
                self.poll_ms = (self.poll_ms + 50).min(500);
                eprintln!("[CAL] overhead {}us > target {}us → poll {}ms→{}ms",
                    self.tick_avg_us, target_overhead, old, self.poll_ms);
            } else if self.tick_avg_us < target_overhead / 2 && self.poll_ms > 100 && self.tick_samples > 20 {
                let old = self.poll_ms;
                self.poll_ms = (self.poll_ms - 50).max(100);
                eprintln!("[CAL] overhead {}us < target/2 → poll {}ms→{}ms",
                    self.tick_avg_us, old, self.poll_ms);
            }
        }
    }

    fn record_detect(&mut self, us: u64) {
        if self.detect_samples == 0 { self.detect_avg_us = us; }
        else { self.detect_avg_us = (self.detect_avg_us * self.detect_samples + us) / (self.detect_samples + 1); }
        self.detect_samples += 1;
        if self.detect_samples == 10 {
            eprintln!("[CAL] detect avg={}us samples={}", self.detect_avg_us, self.detect_samples);
        }
    }

    fn record_jitter(&mut self, drop: u64) {
        if drop > self.max_jitter_drop {
            let old = self.drop_thresh;
            self.max_jitter_drop = drop;
            self.drop_thresh = (drop as f64 * 1.5) as u64;
            eprintln!("[CAL] jitter max={}ms drop={}ms→{}ms (×1.5)", drop, old, self.drop_thresh);
        }
    }

    fn record_stop(&mut self, timeout: u64) {
        let now = std::time::Instant::now();
        let since = self.last_stop_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(999.0);
        if since < 30.0 {
            self.cycle_speed = (self.cycle_speed * self.cycles + since as u64) / (self.cycles + 1);
            self.cycles += 1;
            let new_nz = (self.nz_curr + timeout).min(timeout * 2);
            if new_nz != self.nz_curr {
                eprintln!("[CAL] cycle#{} gap={}s speed_avg={}s nz={}→{} (jump by timeout={})",
                    self.cycles, since as u64, self.cycle_speed, self.nz_curr, new_nz, timeout);
                self.nz_curr = new_nz;
            }
        } else {
            if self.cycles > 0 {
                eprintln!("[CAL] calm gap={}s after {} cycles — nz={}→{}", since as u64, self.cycles, self.nz_curr, self.nz_base);
            }
            self.nz_curr = self.nz_base;
            self.cycles = 0; self.cycle_speed = 0;
        }
        self.last_stop_at = Some(now);
    }
}

fn idle_monitor_loop(app: AppHandle) {
    let mut prev: Option<u64> = None;
    let mut ai: u64 = 0;
    let mut nz: u64 = 0;
    let mut steady: bool = false;
    let mut ac: bool = false;
    let mut cd: u64 = 0;
    let mut jz: u64 = 0; // jitter skip counter
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
        drop(config);
        cal.nz_max = cal.nz_max.max(timeout.min(14));

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
        if det && !mpv && playing {
            let h: Vec<String> = hist.iter().enumerate().map(|(i, &v)| format!("{}:{}", i, v)).collect();
            cd += 1;
            // calibrate: if drop is > our threshold and has happened before, it's jitter not user
            if cal.detect_samples > 5 && drop > cal.max_jitter_drop {
                cal.record_jitter(drop);
            }
            // Jitter envelope: if drop within known jitter ×1.2, skip NZ for enough ticks
            if cal.max_jitter_drop > 0 && drop <= cal.max_jitter_drop * 120 / 100 {
                jz = cal.nz_curr + 4;
                eprintln!("[CAL] jitter_env drop={}ms max_jitter={}ms → skip NZ for {} ticks",
                    drop, cal.max_jitter_drop, jz);
            }
            eprintln!("[idle] DETECT#{}=drop={}ms hist=[{}] prev={:?} curr={}", cd, drop, h.join(","), p, idle_ms);
        } else { cd = 0; }

        if det && mpv { nz = 0; ac = false; prev = Some(idle_ms); ai = 0; continue; }
        if playing && !det && s >= 1 { nz = 0; ac = true; prev = Some(idle_ms); ai = 0; continue; }

        if playing && s < 1 && !mpv {
            if !steady { prev = Some(idle_ms); ai = 0; continue; }
            if ac { ac = false; prev = Some(idle_ms); ai = 0; continue; }
            if jz > 0 {
                jz -= 1;
                if jz == 0 { eprintln!("[CAL] jitter_skip expired — NZ counting resumed"); }
                prev = Some(idle_ms); ai = 0; continue;
            }
            nz += 1; prev = Some(idle_ms); ai = 0;
            if nz >= cal.nz_curr {
                cal.record_stop(timeout);
                eprintln!("[idle] NZ-STOP idle={}ms nz={}/{}", idle_ms, nz, cal.nz_curr);
                nz = 0; ac = false;
                IS_PLAYING.store(false, Ordering::SeqCst);
                let _ = app.emit("playback-stopped", ());
                stop_playback(&app);
            }
            continue;
        }

        nz = 0; jz = 0; prev = Some(idle_ms);

        if s >= timeout {
            ac = false;
            if ai == 0 && !playing { eprintln!("[idle] climbing r={}s idle={}ms", rem, idle_ms); }
            ai += 1;
            if ai >= 2 && autoplay && !playing {
                eprintln!("[idle] AUTOPLAY videos={} rotate={:?}", videos.len(), lpc);
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
            eprintln!("[TIMING] tick {}ms poll={}ms overhead={}ms jz={} nz={} dt={} cyc={}",
                tus / 1000, cal.poll_ms, overhead_us / 1000,
                jz, cal.nz_curr, cal.drop_thresh, cal.cycles);
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
