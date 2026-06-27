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
    pub fn idle_ms() -> Result<u64, IdleError> {
        if let Ok(ms) = Self::via_xprintidle() {
            return Ok(ms);
        }
        if let Ok(ms) = Self::via_gnome_mutter() {
            return Ok(ms);
        }
        Err(IdleError::NoMethod)
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
