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
    let config = state.config.lock().unwrap();
    let videos = config.videos.clone();
    let global = config.global.default_params.clone();
    let last_played = config.global.last_played_entry;
    eprintln!("[daydream] manual_play: videos in order ({}) {:?}",
        videos.len(),
        videos.iter().enumerate().map(|(i, v)| format!("{}:{}", i, v.filename)).collect::<Vec<_>>());
    drop(config);

    if videos.is_empty() {
        return Err("No videos in loop".into());
    }

    let rotate_to = if selected_ids.is_empty() {
        eprintln!("[daydream] play: no selection, resume from last_played={:?}", last_played);
        last_played
    } else {
        let found: Vec<usize> = videos.iter()
            .enumerate()
            .filter(|(_, v)| selected_ids.contains(&v.id))
            .map(|(i, _)| i)
            .collect();
        eprintln!("[daydream] play: selected_ids={:?} found item_indices={:?}", selected_ids, found);
        found.into_iter().min()
    };

    IS_PLAYING.store(true, Ordering::SeqCst);
    start_playback(&app, &videos, &global, rotate_to).map_err(|e| e.to_string())?;
    let _ = app.emit("playback-started", ());
    Ok(())
}

#[tauri::command]
fn manual_stop(app: AppHandle) -> Result<(), String> {
    IS_PLAYING.store(false, Ordering::SeqCst);
    stop_playback(&app);
    let _ = app.emit("playback-stopped", ());
    Ok(())
}

fn idle_monitor_loop(app: AppHandle) {
    let mut previous_idle: Option<u64> = None;
    let mut consec_idle_for_autoplay: u64 = 0;
    let mut near_zero_count: u64 = 0;
    let mut suppressed_count: u64 = 0;
    let mut steady_seen: bool = false;
    let mut after_climb: bool = false;

    while IDLE_MONITOR_ACTIVE.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_secs(1));

        let state = app.state::<ConfigState>();
        let config = state.config.lock().unwrap();
        let timeout = config.global.idle_timeout_seconds;
        let enabled = config.global.idle_enabled;
        let should_autoplay = config.global.autoplay_on_idle;
        let videos = config.videos.clone();
        let global_params = config.global.default_params.clone();
        let last_played_from_config = config.global.last_played_entry;
        let is_playing = IS_PLAYING.load(Ordering::SeqCst);
        eprintln!("[daydream-idle] config loaded: last_played_from_config={:?} timeout={} enabled={} autoplay={} videos={}",
            last_played_from_config, timeout, enabled, should_autoplay, videos.len());
        drop(config);

        if !enabled || videos.is_empty() {
            eprintln!("[daydream-idle] idle disabled or no videos — resetting state");
            previous_idle = None;
            consec_idle_for_autoplay = 0;
            near_zero_count = 0;
            suppressed_count = 0;
            steady_seen = false;
            after_climb = false;
            continue;
        }

        let idle_ms = match idle::IdleDetector::idle_ms() {
            Ok(ms) => ms,
            Err(e) => {
                eprintln!("[daydream-idle] idle detection error: {e} — resetting state");
                previous_idle = None;
                consec_idle_for_autoplay = 0;
                near_zero_count = 0;
                suppressed_count = 0;
                steady_seen = false;
                after_climb = false;
                continue;
            }
        };

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
            eprintln!("[daydream-idle] STEADY idle_secs={} >= timeout={} — timer verified, nz counting enabled", idle_secs, timeout);
        }

        let remaining = timeout.saturating_sub(idle_secs.min(timeout));
        eprintln!("[daydream-idle] tick idle_ms={} idle_secs={} prev={:?} remaining={} timeout={} is_playing={} detected={} mpv_flag={} near_zero={} climbing={} steady={} nz_count={} after_climb={} consec_idle={} suppressed={}",
            idle_ms, idle_secs, prev, remaining, timeout, is_playing, detected_interaction, mpv_flag, near_zero, timer_climbing, steady_seen, near_zero_count, after_climb, consec_idle_for_autoplay, suppressed_count);
        let _ = app.emit("idle-status", serde_json::json!({ "remaining": remaining }));

        // === PATH 1: mpv transition — known false positive, skip ===
        if detected_interaction && mpv_flag {
            suppressed_count += 1;
            near_zero_count = 0;
            after_climb = false;
            eprintln!("[daydream-idle] SUPPRESS drop {}→{} ms from mpv transition (total={})",
                prev.unwrap_or(0), idle_ms, suppressed_count);
            previous_idle = Some(idle_ms);
            consec_idle_for_autoplay = 0;
            continue;
        }

        // === PATH 2: timer climbing after reset — natural recovery, not user ===
        if is_playing && timer_climbing {
            near_zero_count = 0;
            after_climb = true;
            eprintln!("[daydream-idle] CLIMB idle_secs={} — after_climb→true", idle_secs);
            previous_idle = Some(idle_ms);
            consec_idle_for_autoplay = 0;
            continue;
        }

        // === PATH 3: near-zero while playing — only count if sensor proved reliable ===
        if is_playing && near_zero && !mpv_flag {
            if !steady_seen {
                eprintln!("[daydream-idle] UNSTEADY idle_ms={} near_zero but timer NEVER reached timeout={} — NOT counted (sensor may be stuck)",
                    idle_ms, timeout);
                previous_idle = Some(idle_ms);
                consec_idle_for_autoplay = 0;
                continue;
            }

            // Skip first NEAR-ZERO after a CLIMB (timer spontaneously collapsed from noise)
            if after_climb {
                after_climb = false;
                eprintln!("[daydream-idle] POST-CLIMB idle_ms={} after_climb→false — collapse skipped, nz_count stays={}", idle_ms, near_zero_count);
                previous_idle = Some(idle_ms);
                consec_idle_for_autoplay = 0;
                continue;
            }

            near_zero_count += 1;
            eprintln!("[daydream-idle] NEAR-ZERO idle_ms={} steady={} nz_count={}/4",
                idle_ms, steady_seen, near_zero_count);
            previous_idle = Some(idle_ms);
            consec_idle_for_autoplay = 0;

            if near_zero_count >= 4 {
                near_zero_count = 0;
                suppressed_count = 0;
                after_climb = false;
                eprintln!("[daydream-idle] NEAR-ZERO-STOP idle_ms={} sustained for 4 ticks — stopping playback", idle_ms);
                IS_PLAYING.store(false, Ordering::SeqCst);
                let _ = app.emit("playback-stopped", ());
                stop_playback(&app);
            }
            continue;
        }

        // === NONE OF THE ABOVE: just track autoplay ===
        near_zero_count = 0;
        previous_idle = Some(idle_ms);
        eprintln!("[daydream-idle] passthrough idle_secs={} idle_high={} playing={} — nz_count→0 after_climb={}",
            idle_secs, idle_high_enough, is_playing, after_climb);

        if idle_high_enough {
            after_climb = false;
            consec_idle_for_autoplay += 1;
            if consec_idle_for_autoplay >= 2 && should_autoplay && !is_playing {
                eprintln!("[daydream-idle] AUTOPLAY last_played_from_config={:?} rotate_to={:?} videos ({}) order: {:?}",
                    last_played_from_config, last_played_from_config, videos.len(),
                    videos.iter().enumerate().map(|(i, v)| format!("{}:{}", i, v.filename)).collect::<Vec<_>>());
                IS_PLAYING.store(true, Ordering::SeqCst);
                let _ = app.emit("playback-started", ());
                if let Err(e) = start_playback(&app, &videos, &global_params, last_played_from_config) {
                    eprintln!("[daydream-idle] autoplay failed: {e}");
                }
            }
        } else {
            consec_idle_for_autoplay = 0;
        }
    }

    eprintln!("[daydream-idle] monitor loop exiting");
    if IS_PLAYING.load(Ordering::SeqCst) {
        eprintln!("[daydream-idle] cleanup stopping playback on loop exit");
        IS_PLAYING.store(false, Ordering::SeqCst);
        stop_playback(&app);
    }
}

fn start_playback(app: &AppHandle, videos: &[config::VideoItem], global: &config::VideoParams,
    rotate_to: Option<usize>) -> Result<(), player::PlayerError> {
    let player = app.state::<MpvPlayer>();
    player.start_with_monitor(videos, global, rotate_to, app.clone())
}

fn stop_playback(app: &AppHandle) {
    let entry = LAST_PLAYED_ENTRY_MS.load(Ordering::SeqCst);
    eprintln!("[daydream] stop_playback: LAST_PLAYED_ENTRY_MS={} will be saved to config", entry);
    if entry >= 0 {
        let state = app.state::<ConfigState>();
        let mut config = state.config.lock().unwrap();
        config.global.last_played_entry = Some(entry as usize);
        eprintln!("[daydream] stop_playback: saving last_played_entry={:?} to config.json", config.global.last_played_entry);
        let _ = state.save(&config);
    }
    let player = app.state::<MpvPlayer>();
    player.stop();
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
