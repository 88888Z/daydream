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

fn idle_monitor_loop(app: AppHandle) {
    let mut previous_idle: Option<u64> = None;
    let mut consec_idle_for_autoplay: u64 = 0;
    let mut near_zero_count: u64 = 0;
    let mut steady_seen: bool = false;
    let mut nz_threshold: u64 = 4;
    let mut last_stop: Option<std::time::Instant> = None;
    let mut after_climb: bool = false;
    let mut consec_detected: u64 = 0;
    // Ring buffer of last 10 idle_ms values for jitter diagnosis
    let mut hist: [u64; 10] = [0; 10];
    let mut hist_pos: usize = 0;

    while IDLE_MONITOR_ACTIVE.load(Ordering::SeqCst) {
        let _tick_start = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(200));

        let state = app.state::<ConfigState>();
        let config = state.config.lock().unwrap();
        let timeout = config.global.idle_timeout_seconds;
        let enabled = config.global.idle_enabled;
        let should_autoplay = config.global.autoplay_on_idle;
        let videos = config.videos.clone();
        let global_params = config.global.default_params.clone();
        let last_played_from_config = config.global.last_played_entry;
        let is_playing = IS_PLAYING.load(Ordering::SeqCst);
        drop(config);

        if !enabled || videos.is_empty() {
            previous_idle = None;
            consec_idle_for_autoplay = 0;
            near_zero_count = 0;
            steady_seen = false;
            after_climb = false;
            hist_pos = 0; consec_detected = 0;
            continue;
        }

        let idle_ms = match idle::IdleDetector::idle_ms() {
            Ok(ms) => ms,
            Err(e) => {
                eprintln!("[idle] detect error: {e} — resetting");
                previous_idle = None;
                consec_idle_for_autoplay = 0;
                near_zero_count = 0;
                steady_seen = false;
                after_climb = false;
                hist_pos = 0; consec_detected = 0;
                continue;
            }
        };

        // Store in ring buffer
        hist[hist_pos] = idle_ms;
        hist_pos = (hist_pos + 1) % hist.len();

        let idle_secs = idle_ms / 1000;
        let prev = previous_idle;
        let detected_interaction = prev
            .map(|p| p.saturating_sub(idle_ms) > 2000)
            .unwrap_or(false);
        let mpv_flag = MPV_JUST_TRANSITIONED.swap(false, Ordering::SeqCst);

        let near_zero = idle_secs < 1;
        let timer_climbing = !detected_interaction && idle_secs >= 1;
        let idle_high_enough = idle_secs >= timeout;

        if !steady_seen && idle_high_enough {
            steady_seen = true;
            eprintln!("[idle] steady idle_secs={} >= timeout={}", idle_secs, timeout);
        }

        let remaining = timeout.saturating_sub(idle_secs.min(timeout));
        let _ = app.emit("idle-status", serde_json::json!({ "remaining": remaining }));

        // PATH 0: interaction detected — dump history, then fall through to NZ counting
        if detected_interaction && !mpv_flag && is_playing {
            consec_detected += 1;
            let h: Vec<String> = hist.iter().enumerate().map(|(i, &v)| format!("{}:{}", i, v)).collect();
            eprintln!("[idle] DETECT hist=[{}] prev={:?} curr={} consec={}", h.join(","), prev, idle_ms, consec_detected);
            // Fall through to NZ counting — don't stop instantly, let NZ threshold handle it
        }

        // PATH 1: mpv transition
        if detected_interaction && mpv_flag {
            near_zero_count = 0; after_climb = false;
            previous_idle = Some(idle_ms);
            consec_idle_for_autoplay = 0;
            continue;
        }

        // PATH 2: timer climbing
        if is_playing && timer_climbing {
            near_zero_count = 0; after_climb = true;
            previous_idle = Some(idle_ms);
            consec_idle_for_autoplay = 0;
            continue;
        }

        // PATH 3: near-zero while playing
        if is_playing && near_zero && !mpv_flag {
            if !steady_seen {
                previous_idle = Some(idle_ms);
                consec_idle_for_autoplay = 0;
                continue;
            }
            if after_climb {
                after_climb = false;
                previous_idle = Some(idle_ms);
                consec_idle_for_autoplay = 0;
                continue;
            }
            near_zero_count += 1;
            previous_idle = Some(idle_ms);
            consec_idle_for_autoplay = 0;
            if near_zero_count >= nz_threshold {
                // Adaptive threshold: if stops happen rapidly, increase threshold to break the cycle
                let since_last = last_stop.map(|t| t.elapsed().as_secs()).unwrap_or(99);
                if since_last < 10 {
                    nz_threshold = (nz_threshold + 2).min(14);
                    eprintln!("[idle] NZ-STOP idle={}ms nz={}/{} fast_cycle={}s threshold→{}",
                        idle_ms, near_zero_count, nz_threshold, since_last, nz_threshold);
                } else {
                    nz_threshold = 4;
                    eprintln!("[idle] NZ-STOP idle={}ms nz={}/{}", idle_ms, near_zero_count, nz_threshold);
                }
                last_stop = Some(std::time::Instant::now());
                near_zero_count = 0; after_climb = false;
                IS_PLAYING.store(false, Ordering::SeqCst);
                let _ = app.emit("playback-stopped", ());
                stop_playback(&app);
            }
            continue;
        }

        // passthrough: only log climbing progression, silent otherwise
        near_zero_count = 0;
        previous_idle = Some(idle_ms);

        if idle_high_enough {
            after_climb = false;
            if consec_idle_for_autoplay == 0 && !is_playing {
                eprintln!("[idle] climbing r={}s idle={}ms", remaining, idle_ms);
            }
            consec_idle_for_autoplay += 1;
            if consec_idle_for_autoplay >= 2 && should_autoplay && !is_playing {
                eprintln!("[idle] AUTOPLAY videos={} rotate={:?}", videos.len(), last_played_from_config);
                IS_PLAYING.store(true, Ordering::SeqCst);
                let _ = app.emit("playback-started", ());
                let app_clone = app.clone();
                let v = videos.clone();
                let p = global_params.clone();
                std::thread::spawn(move || {
                    if let Err(e) = start_playback(&app_clone, &v, &p, last_played_from_config) {
                        eprintln!("[idle] autoplay failed: {e}");
                    }
                });
            }
        } else {
            consec_idle_for_autoplay = 0;
        }

        // slow tick monitor
        let tick_us = _tick_start.elapsed().as_micros();
        if tick_us > 300_000 {
            eprintln!("[TIMING] tick {}ms overhead {}ms", tick_us / 1000, tick_us.saturating_sub(200_000) / 1000);
        }
    }

    if IS_PLAYING.load(Ordering::SeqCst) {
        IS_PLAYING.store(false, Ordering::SeqCst);
        stop_playback(&app);
    }
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
