use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, State};

#[derive(Debug, Snafu)]
pub enum ConfigError {
    #[snafu(display("Failed to determine config directory"))]
    ConfigDir,
    #[snafu(display("Failed to read config: {source}"))]
    Read { source: std::io::Error },
    #[snafu(display("Failed to write config: {source}"))]
    Write { source: std::io::Error },
    #[snafu(display("Failed to parse config: {source}"))]
    Parse {
        source: serde_json::Error,
    },
    #[snafu(display("Failed to serialize config: {source}"))]
    Serialize {
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoParams {
    pub repeats: u32,
    pub speed: f64,
    pub volume: f64,
}

impl Default for VideoParams {
    fn default() -> Self {
        Self {
            repeats: 1,
            speed: 1.0,
            volume: 100.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoItem {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub local: Option<VideoParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddVideosResult {
    pub items: Vec<VideoItem>,
    pub added: usize,
    pub duplicates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    pub default_params: VideoParams,
    pub idle_timeout_seconds: u64,
    pub idle_enabled: bool,
    pub autoplay_on_idle: bool,
    pub start_on_boot: bool,
    pub last_played_entry: Option<usize>,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            default_params: VideoParams::default(),
            idle_timeout_seconds: 120,
            idle_enabled: false,
            autoplay_on_idle: true,
            start_on_boot: false,
            last_played_entry: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub global: GlobalSettings,
    pub videos: Vec<VideoItem>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            global: GlobalSettings::default(),
            videos: Vec::new(),
        }
    }
}

pub struct ConfigState {
    pub config: Mutex<AppConfig>,
    path: PathBuf,
}

impl ConfigState {
    pub fn new() -> Result<Self, ConfigError> {
        let config_dir = dirs::config_dir()
            .map(|p| p.join("daydream"))
            .ok_or(ConfigError::ConfigDir)?;

        std::fs::create_dir_all(&config_dir).ok();

        let config_path = config_dir.join("config.json");
        let config = if config_path.exists() {
            let data = std::fs::read_to_string(&config_path).context(ReadSnafu)?;
            serde_json::from_str(&data).context(ParseSnafu)?
        } else {
            let default = AppConfig::default();
            let data = serde_json::to_string_pretty(&default).context(SerializeSnafu)?;
            std::fs::write(&config_path, &data).context(WriteSnafu)?;
            default
        };

        Ok(Self {
            config: Mutex::new(config),
            path: config_path,
        })
    }

    pub fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        let data = serde_json::to_string_pretty(config).context(SerializeSnafu)?;
        std::fs::write(&self.path, &data).context(WriteSnafu)?;
        Ok(())
    }
}

#[tauri::command]
pub fn get_config(state: State<ConfigState>) -> Result<AppConfig, String> {
    let config = state.config.lock().unwrap().clone();
    Ok(config)
}

#[tauri::command]
pub fn update_global_settings(
    state: State<ConfigState>,
    settings: GlobalSettings,
) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    config.global = settings;
    state.save(&config).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn add_videos(app: tauri::AppHandle, state: State<ConfigState>, paths: Vec<String>) -> Result<AddVideosResult, String> {
    let total = paths.len();
    let config = state.config.lock().unwrap();
    let existing_paths: HashSet<&String> = config.videos.iter().map(|v| &v.path).collect();
    let new_paths: Vec<String> = paths.into_iter().filter(|p| !existing_paths.contains(p)).collect();
    let duplicates = total - new_paths.len();
    let added = new_paths.len();
    drop(config);

    if new_paths.is_empty() {
        return Ok(AddVideosResult { items: vec![], added: 0, duplicates });
    }

    let mut items = Vec::with_capacity(added);
    for (i, path) in new_paths.into_iter().enumerate() {
        let filename = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        items.push(VideoItem {
            id: uuid::Uuid::new_v4().to_string(),
            path,
            filename,
            local: None,
        });
        let _ = app.emit("add-progress", serde_json::json!({
            "current": i + 1,
            "total": total,
        }));
    }

    let cloned = items.clone();
    let mut config = state.config.lock().unwrap();
    config.videos.extend(items);
    state.save(&config).map_err(|e| e.to_string())?;

    let _ = app.emit("add-progress", serde_json::json!({
        "current": total,
        "total": total,
    }));
    eprintln!("[daydream-config] add_videos: added={} duplicates={} total={}", added, duplicates, total);
    Ok(AddVideosResult { items: cloned, added, duplicates })
}

#[tauri::command]
pub fn remove_video(state: State<ConfigState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    config.videos.retain(|v| v.id != id);
    state.save(&config).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn reorder_videos(state: State<ConfigState>, ids: Vec<String>) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    let mut ordered = Vec::with_capacity(ids.len());
    eprintln!("[daydream-config] reorder_videos: received {} ids", ids.len());
    for (pos, id) in ids.iter().enumerate() {
        if let Some(item) = config.videos.iter().find(|v| &v.id == id) {
            eprintln!("[daydream-config] reorder  pos={} filename={} id={}", pos, item.filename, id);
            ordered.push(item.clone());
        } else {
            eprintln!("[daydream-config] reorder  pos={} id={} NOT FOUND in config.videos!", pos, id);
        }
    }
    eprintln!("[daydream-config] reorder_videos: old order {:?}", config.videos.iter().map(|v| v.filename.clone()).collect::<Vec<_>>());
    config.videos = ordered;
    let new_order: Vec<String> = config.videos.iter().map(|v| v.filename.clone()).collect();
    eprintln!("[daydream-config] reorder_videos: new order {:?}", new_order);
    state.save(&config).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn update_video_params(
    state: State<ConfigState>,
    id: String,
    params: Option<VideoParams>,
) -> Result<(), String> {
    let mut config = state.config.lock().unwrap();
    if let Some(item) = config.videos.iter_mut().find(|v| v.id == id) {
        item.local = params;
    }
    state.save(&config).map_err(|e| e.to_string())?;
    Ok(())
}
