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
        let mut paths: Vec<String> = Vec::new();
        let mut map: Vec<usize> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            let params = item.local.as_ref().unwrap_or(global);
            for _ in 0..params.repeats.max(1) {
                paths.push(item.path.clone());
                map.push(idx);
            }
        }
        (paths, map)
    }

    fn spawn_mpv(
        &self,
        expanded_paths: &[String],
    ) -> Result<(Child, PathBuf), PlayerError> {
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

        let mut args: Vec<&str> = vec![
            "--input-ipc-server=/tmp/daydream-mpv.sock",
            "--fullscreen",
            "--no-border",
            "--vo=gpu-next",
            "--keep-open=yes",
            "--loop-playlist=inf",
        ];
        for p in expanded_paths.iter() {
            args.push(p.as_str());
        }

        let child = Command::new("mpv")
            .env("WAYLAND_DISPLAY", std::env::var("WAYLAND_DISPLAY").unwrap_or_default())
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| PlayerError::Spawn { source: e })?;

        Ok((child, socket_path))
    }

    fn connect_event_reader(
        stop_signal: Arc<AtomicBool>,
        entry_to_item: Vec<usize>,
        total_items: usize,
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
                for line in reader.lines() {
                    if stop_signal.load(Ordering::SeqCst) {
                        break;
                    }
                    match line {
                        Ok(json) => {
                            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json) {
                                if obj.get("event").and_then(|e| e.as_str()) == Some("playback-restart") {
                                    if let Some(entry_id) = obj.get("playlist_entry_id").and_then(|e| e.as_u64()) {
                                        let idx = (entry_id as usize).saturating_sub(1);
                                        if let Some(&item_idx) = entry_to_item.get(idx) {
                                            let _ = app.emit("now-playing", serde_json::json!({
                                                "itemIndex": item_idx,
                                                "entryIndex": idx,
                                                "totalEntries": total_items,
                                            }));
                                        }
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
        app: tauri::AppHandle,
    ) -> Result<(), PlayerError> {
        if items.is_empty() {
            return Ok(());
        }

        if self.is_running() {
            self.stop();
        }

        let (expanded_paths, entry_to_item) = Self::build_expanded_paths(items, global);
        let total_items = expanded_paths.len();

        let (child, socket_path) = self.spawn_mpv(&expanded_paths)?;

        let stop_signal = Arc::new(AtomicBool::new(false));
        *self.stop_signal.lock().unwrap() = Some(stop_signal.clone());
        *self.child.lock().unwrap() = Some(child);

        // Connect event reader as soon as socket appears (before IPC commands)
        let mut reader_spawned = false;
        for _ in 0..30 {
            if socket_path.exists() {
                if !reader_spawned {
                    Self::connect_event_reader(
                        stop_signal.clone(),
                        entry_to_item.clone(),
                        total_items,
                        app.clone(),
                    );
                    reader_spawned = true;
                }
                std::thread::sleep(Duration::from_millis(100));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !reader_spawned {
            self.stop();
            return Err(PlayerError::SocketTimeout);
        }

        // Small extra wait for reader to be ready for events
        std::thread::sleep(Duration::from_millis(200));

        // Send baseline now-playing (item 0) in case first event was already fired
        let _ = app.emit("now-playing", serde_json::json!({
            "itemIndex": 0usize,
            "entryIndex": 0usize,
            "totalEntries": total_items,
        }));

        // Set speed/volume for first item (non-critical, can fail)
        if let Some(first) = items.first() {
            let params = first.local.as_ref().unwrap_or(global);
            let _ = self.send_json(&serde_json::json!(["set", "speed", params.speed]));
            let _ = self.send_json(&serde_json::json!(["set", "volume", params.volume]));
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

        Ok(())
    }

    fn send_json(&self, cmd: &serde_json::Value) -> Result<(), PlayerError> {
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
        if let Some(signal) = self.stop_signal.lock().unwrap().take() {
            signal.store(true, Ordering::SeqCst);
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        std::thread::sleep(Duration::from_millis(100));
        let _ = std::fs::remove_file(PathBuf::from(MPV_SOCKET));
    }
}
