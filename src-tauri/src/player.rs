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

            // Retry connect up to 10× (1s total)
            let mut stream = None;
            for _ in 0..10 {
                if stop_signal.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(s) = UStream::connect(MPV_SOCKET) {
                    stream = Some(s);
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }

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
                                let event_name = obj.get("event").and_then(|e| e.as_str()).unwrap_or("");
                                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();

                                // Log all key events with timestamps for transition latency tracking
                                if event_name == "end-file" || event_name == "start-file" || event_name == "playback-restart" {
                                    let eid = obj.get("playlist_entry_id").and_then(|e| e.as_u64()).unwrap_or(0);
                                    eprintln!("[TIMING] mpv_event={} t={}ms entry_id={}", event_name, now_ms, eid);
                                }

                                // === start-file: carries playlist_entry_id, store immediately ===
                                if event_name == "start-file" {
                                    last_entry_id = obj.get("playlist_entry_id").and_then(|e| e.as_u64());
                                    if let Some(eid) = last_entry_id {
                                        let idx = (eid as usize).saturating_sub(1);
                                        crate::LAST_PLAYED_ENTRY_MS.store(idx as i64, std::sync::atomic::Ordering::SeqCst);
                                        if let Some(&item_idx) = entry_to_item.get(idx) {
                                            let _ = app.emit("now-playing", serde_json::json!({
                                                "itemIndex": item_idx,
                                                "entryIndex": idx,
                                                "totalEntries": total_items,
                                            }));
                                        }
                                    }
                                }

                                // === playback-restart: set transition flag, apply speed/volume, emit now-playing ===
                                if event_name == "playback-restart" {
                                    crate::MPV_JUST_TRANSITIONED.store(true, std::sync::atomic::Ordering::SeqCst);
                                    crate::LAST_MPV_TRANSITION_MS.store(now_ms as i64, std::sync::atomic::Ordering::SeqCst);

                                    let eid = last_entry_id.unwrap_or(1);
                                    let idx = (eid as usize).saturating_sub(1);
                                    crate::LAST_PLAYED_ENTRY_MS.store(idx as i64, std::sync::atomic::Ordering::SeqCst);

                                    // Apply speed/volume for the current video
                                    if let Some(&item_idx) = entry_to_item.get(idx) {
                                        if let Some(video) = items.get(item_idx) {
                                            let params = video.local.as_ref().unwrap_or(&global);
                                            let _ = Self::send_cmd(&serde_json::json!({"command": ["set_property", "speed", params.speed]}));
                                            let _ = Self::send_cmd(&serde_json::json!({"command": ["set_property", "volume", params.volume]}));
                                        }
                                        let _ = app.emit("now-playing", serde_json::json!({
                                            "itemIndex": item_idx,
                                            "entryIndex": idx,
                                            "totalEntries": total_items,
                                        }));
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
            eprintln!("[TIMING] start_with_monitor {}us items=0 EMPTY", _t.elapsed().as_micros());
            return Ok(());
        }
        let mut stop_time: u64 = 0;

        if self.is_running() {
            let _stop_t = std::time::Instant::now();
            self.stop();
            stop_time = _stop_t.elapsed().as_micros() as u64;
        }

        let (expanded_paths, entry_to_item) = Self::build_expanded_paths(items, global);
        let total_items = expanded_paths.len();

        let rotate_to_entry = rotate_to
            .and_then(|item_idx| {
                let _r_t = std::time::Instant::now();
                let pos = entry_to_item.iter().position(|&i| i == item_idx);
                eprintln!("[TIMING] resolve_rotate {}us item_idx={} pos={:?}", _r_t.elapsed().as_micros(), item_idx, pos);
                pos
            });

        let _spawn_t = std::time::Instant::now();
        let (child, socket_path) = self.spawn_mpv(&expanded_paths, rotate_to_entry)?;
        let spawn_us = _spawn_t.elapsed().as_micros();

        let stop_signal = Arc::new(AtomicBool::new(false));
        *self.stop_signal.lock().unwrap() = Some(stop_signal.clone());
        *self.child.lock().unwrap() = Some(child);

        // Connect event reader as soon as socket appears (before IPC commands)
        let _reader_t = std::time::Instant::now();
        let mut reader_spawned = false;
        for _ in 0..30 {
            if socket_path.exists() {
                if !reader_spawned {
                    Self::connect_event_reader(
                        stop_signal.clone(),
                        entry_to_item.clone(),
                        total_items,
                        items.to_vec(),
                        global.clone(),
                        app.clone(),
                    );
                    reader_spawned = true;
                }
                std::thread::sleep(Duration::from_millis(100));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let reader_us = _reader_t.elapsed().as_micros();
        if !reader_spawned {
            self.stop();
            return Err(PlayerError::SocketTimeout);
        }

        // Small extra wait for reader to be ready for events
        std::thread::sleep(Duration::from_millis(200));

        // Send baseline now-playing with correct item/entry
        let baseline_item = rotate_to.unwrap_or(0);
        let baseline_entry = rotate_to_entry.unwrap_or(0);
        let _ = app.emit("now-playing", serde_json::json!({
            "itemIndex": baseline_item,
            "entryIndex": baseline_entry,
            "totalEntries": total_items,
        }));

        // Set speed/volume for first item
        if let Some(first) = items.first() {
            let params = first.local.as_ref().unwrap_or(global);
            let _ = Self::send_cmd(&serde_json::json!({"command": ["set_property", "speed", params.speed]}));
            let _ = Self::send_cmd(&serde_json::json!({"command": ["set_property", "volume", params.volume]}));
        }

        let child_for_monitor = self.child.lock().unwrap().take();
        let app_clone = app.clone();

        // Process monitor – waits for mpv to exit
        std::thread::spawn(move || {
            let mut child = child_for_monitor;
            let mut exited = false;

            while let Some(ref mut c) = child {
                if stop_signal.load(Ordering::SeqCst) {
                    let _ = c.kill();
                    let _ = c.wait();
                    exited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
                match c.try_wait() {
                    Ok(Some(_)) => {
                        let _ = c.wait();
                        exited = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        exited = true;
                        break;
                    }
                }
            }

            let _ = std::fs::remove_file(PathBuf::from(MPV_SOCKET));
            if exited {
                crate::IS_PLAYING.store(false, Ordering::SeqCst);
                let _ = app_clone.emit("playback-stopped", ());
            }
        });

        eprintln!("[TIMING] start_with_monitor {}us spawn={}us reader={}us stop={}us items={} expanded={}",
            _t.elapsed().as_micros(), spawn_us, reader_us, stop_time, items.len(), expanded_paths.len());
        Ok(())
    }

    fn send_cmd(cmd: &serde_json::Value) -> Result<(), PlayerError> {
        let _t = std::time::Instant::now();
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
        eprintln!("[TIMING] send_cmd {}us", _t.elapsed().as_micros());
        Ok(())
    }

    pub fn stop(&self) {
        let _t = std::time::Instant::now();
        if let Some(signal) = self.stop_signal.lock().unwrap().take() {
            signal.store(true, Ordering::SeqCst);
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        std::thread::sleep(Duration::from_millis(100));
        let _ = std::fs::remove_file(PathBuf::from(MPV_SOCKET));
        eprintln!("[TIMING] mpv_stop {}us", _t.elapsed().as_micros());
    }
}
