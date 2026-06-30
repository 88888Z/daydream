use snafu::Snafu;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config;
use tauri::Emitter;
use tauri::Manager;

#[derive(Debug, Snafu)]
pub enum PlayerError {
    Spawn { source: std::io::Error },
    Command { source: std::io::Error },
    NotRunning,
    SocketTimeout,
}

const MPV_SOCKET: &str = "/tmp/daydream-mpv.sock";

pub struct MpvPlayer {
    child: Mutex<Option<Child>>,
    stop_signal: Mutex<Option<Arc<AtomicBool>>>,
}

impl MpvPlayer {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            stop_signal: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().is_none())
    }

    fn build_expanded_paths(
        items: &[config::VideoItem],
        global: &config::VideoParams,
    ) -> (Vec<String>, Vec<usize>) {
        let _t = std::time::Instant::now();
        let mut paths: Vec<String> = Vec::new();
        let mut map: Vec<usize> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            let params = item.local.as_ref().unwrap_or(global);
            for _ in 0..params.repeats.max(1) {
                paths.push(item.path.clone());
                map.push(idx);
            }
        }
        eprintln!("[TIMING] build_expanded_paths {}us items={} expanded={}",
            _t.elapsed().as_micros(), items.len(), paths.len());
        (paths, map)
    }

    fn spawn_mpv(
        &self,
        expanded_paths: &[String],
        rotate_to_entry: Option<usize>,
    ) -> Result<(Child, PathBuf), PlayerError> {
        let _t = std::time::Instant::now();
        let socket_path = PathBuf::from(MPV_SOCKET);
        let _ = std::fs::remove_file(&socket_path);

        for path in expanded_paths.iter() {
            if !std::path::Path::new(path).exists() {
                return Err(PlayerError::Command {
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("File not found: {path}"),
                    ),
                });
            }
        }

        let mut args: Vec<String> = vec![
            "--input-ipc-server=/tmp/daydream-mpv.sock".into(),
            "--fullscreen".into(),
            "--no-border".into(),
            "--vo=gpu-next".into(),
            "--keep-open=yes".into(),
            "--loop-playlist=inf".into(),
        ];
        if let Some(entry) = rotate_to_entry {
            args.push(format!("--playlist-start={entry}"));
        }
        for p in expanded_paths.iter() {
            args.push(p.clone());
        }

        let args_refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();

        let child = Command::new("mpv")
            .env("WAYLAND_DISPLAY", std::env::var("WAYLAND_DISPLAY").unwrap_or_default())
            .args(&args_refs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| PlayerError::Spawn { source: e })?;

        eprintln!("[TIMING] spawn_mpv {}us paths={} rotate={:?}",
            _t.elapsed().as_micros(), expanded_paths.len(), rotate_to_entry);
        Ok((child, socket_path))
    }

    fn connect_event_reader(
        stop_signal: Arc<AtomicBool>,
        entry_to_item: Vec<usize>,
        total_items: usize,
        items: Vec<config::VideoItem>,
        global: config::VideoParams,
        app: tauri::AppHandle,
    ) {
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            use std::os::unix::net::UnixStream as UStream;

            let mut stream = None;
            let mut retries = 0u32;
            for _ in 0..10 {
                if stop_signal.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(s) = UStream::connect(MPV_SOCKET) {
                    stream = Some(s);
                    break;
                }
                retries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("[TIMING] reader_connect {} retries", retries);

            if let Some(stream) = stream {
                let reader = BufReader::new(stream);
                let mut last_entry_id: Option<u64> = None;
                for line in reader.lines() {
                    if stop_signal.load(Ordering::SeqCst) {
                        break;
                    }
                    match line {
                        Ok(json) => {
                            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json) {
                                let event_name =
                                    obj.get("event").and_then(|e| e.as_str()).unwrap_or("");
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis();

                                // Log every event with full context
                                let raw_eid = obj.get("playlist_entry_id").and_then(|e| e.as_u64());
                                if event_name == "end-file"
                                    || event_name == "start-file"
                                    || event_name == "playback-restart"
                                {
                                    eprintln!("[MPV] event={} t={}ms raw_entry={:?}",
                                        event_name, now_ms, raw_eid);
                                }

                                // === end-file: transition is starting (next file will load) ===
                                if event_name == "end-file" {
                                    crate::MPV_JUST_TRANSITIONED.store(
                                        true, std::sync::atomic::Ordering::SeqCst,
                                    );
                                }

                                // === start-file: carries playlist_entry_id, store UUID immediately ===
                                if event_name == "start-file" {
                                    last_entry_id = raw_eid;
                                    eprintln!("[TRACE] start-file last_entry_id={:?} entry_to_item.len={} total_items={}",
                                        last_entry_id, entry_to_item.len(), total_items);
                                    if let Some(eid) = last_entry_id {
                                        let idx = (eid as usize).saturating_sub(1);
                                        let got = entry_to_item.get(idx);
                                        eprintln!("[TRACE] start-file eid={} idx={} got={:?}",
                                            eid, idx, got);
                                        if let Some(&item_idx) = got {
                                            let video = items.get(item_idx);
                                            eprintln!("[TRACE] start-file item_idx={} video={:?}",
                                                item_idx, video.as_ref().map(|v| &v.filename));
                                            if let Some(video) = video {
                                                *crate::LAST_PLAYED_UUID.lock().unwrap() = Some(video.id.clone());
                                                eprintln!("[TRACE] start-file SET LAST_PLAYED_UUID={}", video.id);
                                            }
                                            let _ = app.emit("now-playing", serde_json::json!({
                                                "itemIndex": item_idx,
                                                "entryIndex": idx,
                                                "totalEntries": total_items,
                                            }));
                                        } else {
                                            eprintln!("[TRACE] start-file MISS idx={} >= entry_to_item.len={}",
                                                idx, entry_to_item.len());
                                        }
                                    } else {
                                        eprintln!("[TRACE] start-file raw_eid was None");
                                    }
                                }

                                // === playback-restart: measure startup, apply speed/volume ===
                                // Note: MPV_JUST_TRANSITIONED is NOT set here — mpv fires 2-5
                                // playback-restart events per file on this system, each one resetting
                                // the noise model's signal detection. Only end-file is a reliable
                                // transition signal (fires exactly once per file transition).
                                if event_name == "playback-restart" {
                                    let now_ms_i64 = now_ms as i64;
                                    crate::LAST_MPV_TRANSITION_MS.store(
                                        now_ms_i64, std::sync::atomic::Ordering::SeqCst,
                                    );
                                    crate::LAST_TRANSITION_AT.store(
                                        now_ms_i64, std::sync::atomic::Ordering::Release,
                                    );

                                    // Measure mpv startup delay
                                    let started = crate::MPV_STARTED_AT.load(
                                        std::sync::atomic::Ordering::Acquire,
                                    );
                                    if started > 0 {
                                        let elapsed = (now_ms as i64) - started;
                                        if elapsed > 0 && elapsed < 30000 {
                                            crate::LAST_STARTUP_MS.store(
                                                elapsed, std::sync::atomic::Ordering::Release,
                                            );
                                        }
                                    }

                                    // When last_entry_id is None, this is the initial playback-restart
                                    // before any start-file event was processed. start_with_monitor
                                    // already set LAST_PLAYED_UUID and applied speed/volume for the
                                    // correct video, so skip to avoid overwriting with idx=0.
                                    if let Some(eid) = last_entry_id {
                                        let idx = (eid as usize).saturating_sub(1);
                                        eprintln!("[TRACE] playback-restart last_entry_id={:?} eid={} idx={} entry_to_item.len={}",
                                            last_entry_id, eid, idx, entry_to_item.len());

                                        if let Some(&item_idx) = entry_to_item.get(idx) {
                                            let video = items.get(item_idx);
                                            eprintln!("[TRACE] playback-restart item_idx={} video={:?}",
                                                item_idx, video.as_ref().map(|v| &v.filename));
                                            if let Some(video) = video {
                                                *crate::LAST_PLAYED_UUID.lock().unwrap() = Some(video.id.clone());
                                                eprintln!("[TRACE] playback-restart SET LAST_PLAYED_UUID={}", video.id);
                                                let params = video.local.as_ref().unwrap_or(&global);
                                                let _ = Self::send_cmd(&serde_json::json!(
                                                    {"command": ["set_property", "speed", params.speed]}
                                                ));
                                                let _ = Self::send_cmd(&serde_json::json!(
                                                    {"command": ["set_property", "volume", params.volume]}
                                                ));
                                            }
                                            let _ = app.emit("now-playing", serde_json::json!({
                                                "itemIndex": item_idx,
                                                "entryIndex": idx,
                                                "totalEntries": total_items,
                                            }));
                                        } else {
                                            eprintln!("[TRACE] playback-restart MISS idx={} >= entry_to_item.len={}",
                                                idx, entry_to_item.len());
                                        }
                                    } else {
                                        eprintln!("[TRACE] playback-restart skipped (last_entry_id=None, keeping initial LAST_PLAYED_UUID)");
                                    }
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });
    }

    pub fn start_with_monitor(
        &self,
        items: &[config::VideoItem],
        global: &config::VideoParams,
        rotate_to: Option<usize>,
        app: tauri::AppHandle,
    ) -> Result<(), PlayerError> {
        let _t = std::time::Instant::now();
        if items.is_empty() {
            return Ok(());
        }
        if self.is_running() {
            self.stop();
        }

        let (expanded_paths, entry_to_item) = Self::build_expanded_paths(items, global);
        let total_items = expanded_paths.len();

        let rotate_to_entry = rotate_to
            .and_then(|item_idx| {
                entry_to_item.iter().position(|&i| i == item_idx)
            });

        let (child, _) = self.spawn_mpv(&expanded_paths, rotate_to_entry)?;

        let stop_signal = Arc::new(AtomicBool::new(false));
        *self.stop_signal.lock().unwrap() = Some(stop_signal.clone());
        *self.child.lock().unwrap() = Some(child);

        Self::connect_event_reader(
            stop_signal.clone(),
            entry_to_item.clone(),
            total_items,
            items.to_vec(),
            global.clone(),
            app.clone(),
        );

        let baseline_item = rotate_to.unwrap_or(0);
        let baseline_entry = rotate_to_entry.unwrap_or(0);

        // Set LAST_PLAYED_UUID immediately so the first playback-restart
        // (which has last_entry_id=None and defaults to eid=1 idx=0) doesn't
        // overwrite it with the wrong video.
        if let Some(video) = items.get(baseline_item) {
            *crate::LAST_PLAYED_UUID.lock().unwrap() = Some(video.id.clone());
            let params = video.local.as_ref().unwrap_or(global);
            // Retry send_cmd until mpv creates the IPC socket (usually ~200ms).
            // Otherwise the initial speed/volume is silently lost.
            let speed_cmd = serde_json::json!({"command": ["set_property", "speed", params.speed]});
            let vol_cmd = serde_json::json!({"command": ["set_property", "volume", params.volume]});
            for _ in 0..50 {
                if Self::send_cmd(&speed_cmd).is_ok() {
                    let _ = Self::send_cmd(&vol_cmd);
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let _ = app.emit("now-playing", serde_json::json!({
            "itemIndex": baseline_item,
            "entryIndex": baseline_entry,
            "totalEntries": total_items,
        }));

        let child_for_monitor = self.child.lock().unwrap().take();
        let app_clone = app.clone();

        std::thread::spawn(move || {
            let mut child = child_for_monitor;
            let mut natural_exit = false;

            while let Some(ref mut c) = child {
                if stop_signal.load(Ordering::SeqCst) {
                    let _ = c.kill();
                    let _ = c.wait();
                    natural_exit = false;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
                match c.try_wait() {
                    Ok(Some(_)) => {
                        let _ = c.wait();
                        natural_exit = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        natural_exit = false;
                        break;
                    }
                }
            }

            let _ = std::fs::remove_file(PathBuf::from(MPV_SOCKET));
            if natural_exit {
                eprintln!("[MPV] natural_exit — stopping");
                if let Some(uuid) = crate::LAST_PLAYED_UUID.lock().unwrap().take() {
                    if let Some(state) = app_clone.try_state::<crate::config::ConfigState>() {
                        let mut config = state.config.lock().unwrap();
                        config.global.last_played_id = Some(uuid);
                        let _ = state.save(&config);
                    }
                }
                crate::IS_PLAYING.store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = app_clone.emit("playback-stopped", ());
            }
        });

        Ok(())
    }

    fn send_cmd(cmd: &serde_json::Value) -> Result<(), PlayerError> {
        let socket_path = PathBuf::from(MPV_SOCKET);
        if !socket_path.exists() {
            return Err(PlayerError::NotRunning);
        }
        let mut stream =
            UnixStream::connect(&socket_path).map_err(|_| PlayerError::NotRunning)?;
        let data = serde_json::to_vec(cmd).map_err(|e| PlayerError::Command {
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })?;
        stream.write_all(&data).map_err(|e| PlayerError::Command { source: e })?;
        stream.write_all(b"\n").map_err(|e| PlayerError::Command { source: e })?;
        Ok(())
    }

    pub fn stop(&self) {
        let _t = std::time::Instant::now();
        if let Some(signal) = self.stop_signal.lock().unwrap().take() {
            signal.store(true, Ordering::SeqCst);
        }
        // Don't kill child directly — the monitor thread owns it.
        // Setting the stop signal is enough; the monitor thread will kill it on its
        // next poll (50ms). Directly killing mpv with SIGKILL can leave GPU state
        // dirty, causing the next mpv instance to hang during init.
        let _ = std::fs::remove_file(PathBuf::from(MPV_SOCKET));
        eprintln!("[TIMING] mpv_stop {}us", _t.elapsed().as_micros());
    }
}
