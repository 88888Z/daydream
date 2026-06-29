use std::process::Command;
use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum IdleError {
    #[snafu(display("No idle detection method available"))]
    NoMethod,
    #[snafu(display("Subprocess error: {source}"))]
    Subprocess { source: std::io::Error },
    #[snafu(display("Parse error"))]
    Parse,
}

pub struct IdleDetector;

impl IdleDetector {
    /// Try every available detection method and return the highest (most conservative) value.
    /// Logs every individual reading so we can see which method drove each decision.
    pub fn idle_ms() -> Result<u64, IdleError> {
        let xi = Self::via_xprintidle();
        let mu = Self::via_gnome_mutter();

        let mut best: Option<u64> = None;
        let mut parts: Vec<String> = Vec::new();

        if let Ok(ms) = xi {
            parts.push(format!("xp={}", ms));
            best = best.map(|b| b.max(ms)).or(Some(ms));
        }
        if let Ok(ms) = mu {
            parts.push(format!("mu={}", ms));
            best = best.map(|b| b.max(ms)).or(Some(ms));
        }

        if let Some(ms) = best {
            eprintln!("[idle] {} → {}ms", parts.join(" "), ms);
            Ok(ms)
        } else {
            Err(IdleError::NoMethod)
        }
    }

    fn via_xprintidle() -> Result<u64, IdleError> {
        let output = Command::new("xprintidle")
            .output()
            .map_err(|e| IdleError::Subprocess { source: e })?;
        if !output.status.success() {
            return Err(IdleError::NoMethod);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let ms = text.trim().parse::<u64>().map_err(|_| IdleError::Parse)?;
        Ok(ms)
    }

    fn via_gnome_mutter() -> Result<u64, IdleError> {
        let output = Command::new("dbus-send")
            .args([
                "--print-reply",
                "--dest=org.gnome.Mutter.IdleMonitor",
                "/org/gnome/Mutter/IdleMonitor/Core",
                "org.gnome.Mutter.IdleMonitor.GetIdletime",
            ])
            .output()
            .map_err(|e| IdleError::Subprocess { source: e })?;

        if !output.status.success() {
            return Err(IdleError::NoMethod);
        }

        let text = String::from_utf8_lossy(&output.stdout);
        for token in text.split_whitespace() {
            if let Ok(ms) = token.parse::<u64>() {
                return Ok(ms);
            }
        }
        Err(IdleError::Parse)
    }
}
