mod config;
mod idle;
mod player;
mod thumbnail;

use config::ConfigState;
use player::MpvPlayer;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    AppHandle, Emitter, Manager,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

static IDLE_MONITOR_ACTIVE: AtomicBool = AtomicBool::new(false);
pub static IS_PLAYING: AtomicBool = AtomicBool::new(false);

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
fn manual_play(app: AppHandle, state: tauri::State<ConfigState>) -> Result<(), String> {
    let config = state.config.lock().unwrap();
    let videos = config.videos.clone();
    let global = config.global.default_params.clone();
    drop(config);

    if videos.is_empty() {
        return Err("No videos in loop".into());
    }

    IS_PLAYING.store(true, Ordering::SeqCst);
    start_playback(&app, &videos, &global).map_err(|e| e.to_string())?;
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
    let mut consecutive_idle = 0u64;
    let mut previous_idle: Option<u64> = None;

    while IDLE_MONITOR_ACTIVE.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_secs(1));

        let state = app.state::<ConfigState>();
        let config = state.config.lock().unwrap();
        let timeout = config.global.idle_timeout_seconds;
        let enabled = config.global.idle_enabled;
        let should_autoplay = config.global.autoplay_on_idle;
        let videos = config.videos.clone();
        let global_params = config.global.default_params.clone();
        drop(config);

        if !enabled || videos.is_empty() {
            consecutive_idle = 0;
            previous_idle = None;
            continue;
        }

        match idle::IdleDetector::idle_ms() {
            Ok(idle_ms) => {
                let idle_secs = idle_ms / 1000;

                let detected_interaction = previous_idle
                    .map(|prev| prev.saturating_sub(idle_ms) > 2000)
                    .unwrap_or(false);
                previous_idle = Some(idle_ms);

                if detected_interaction {
                    eprintln!("[daydream-idle] interaction: idle_ms {} → {} (drop >2s)",
                        previous_idle.unwrap_or(0) + idle_ms.saturating_sub(previous_idle.unwrap_or(0)),
                        idle_ms);
                }

                let remaining = timeout.saturating_sub(idle_secs.min(timeout));
                eprintln!("[daydream-idle] emit idle-status: remaining={} idle_ms={} timeout={}",
                    remaining, idle_ms, timeout);
                let _ = app.emit("idle-status", serde_json::json!({ "remaining": remaining }));

                let is_active = idle_secs < timeout || detected_interaction;

                if is_active {
                    if IS_PLAYING.load(Ordering::SeqCst) {
                        eprintln!("[daydream-idle] stopping playback (active)");
                        IS_PLAYING.store(false, Ordering::SeqCst);
                        let _ = app.emit("playback-stopped", ());
                        stop_playback(&app);
                    }
                    consecutive_idle = 0;
                } else {
                    consecutive_idle += 1;
                    if consecutive_idle >= 2 && should_autoplay && !IS_PLAYING.load(Ordering::SeqCst) {
                        eprintln!("[daydream-idle] starting idle playback");
                        IS_PLAYING.store(true, Ordering::SeqCst);
                        let _ = app.emit("playback-started", ());
                        if let Err(e) = start_playback(&app, &videos, &global_params) {
                            eprintln!("[daydream-idle] playback failed: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("[daydream-idle] idle detection error: {e}");
                consecutive_idle = 0;
                previous_idle = None;
            }
        }
    }

    if IS_PLAYING.load(Ordering::SeqCst) {
        IS_PLAYING.store(false, Ordering::SeqCst);
        stop_playback(&app);
    }
}

fn start_playback(app: &AppHandle, videos: &[config::VideoItem], global: &config::VideoParams) -> Result<(), player::PlayerError> {
    let player = app.state::<MpvPlayer>();
    player.start_with_monitor(videos, global, app.clone())
}

fn stop_playback(app: &AppHandle) {
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
