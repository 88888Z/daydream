use md5::{Digest, Md5};
use std::path::PathBuf;

fn encode_uri_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 32);
    for byte in path.bytes() {
        match byte {
            b' ' => out.push_str("%20"),
            b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b'-'
            | b'.' | b'/' | b':' | b';' | b'=' | b'@' | b'_' | b'~'
            | b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

pub fn find_thumbnail(video_path: &str) -> Option<PathBuf> {
    let uri = format!("file://{}", encode_uri_path(video_path));
    let hash = Md5::digest(uri.as_bytes());
    let hex = format!("{:x}", hash);

    let cache_dir = dirs::cache_dir()?;
    let base = cache_dir.join("thumbnails");

    for dir in &["large", "normal"] {
        let path = base.join(dir).join(format!("{hex}.png"));
        if path.exists() {
            return Some(path);
        }
    }

    None
}

#[tauri::command]
pub fn get_thumbnail_path(path: String) -> Result<Option<String>, String> {
    Ok(find_thumbnail(&path).map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn get_thumbnail_base64(path: String) -> Result<Option<String>, String> {
    let thumb_path = find_thumbnail(&path);
    match thumb_path {
        Some(p) => {
            let data = std::fs::read(&p).map_err(|e| format!("Failed to read thumbnail: {e}"))?;
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
            Ok(Some(format!("data:image/png;base64,{b64}")))
        }
        None => Ok(None),
    }
}
