use std::process::Command;
use std::time::Instant;
use snafu::Snafu;

#[derive(Debug, Snafu)]
pub enum IdleError {
    NoMethod,
    Subprocess { source: std::io::Error },
    Parse,
}

#[derive(Debug, Clone)]
pub struct IdleSample {
    pub idle_ms: u64,
    pub method: &'static str,
    pub latency_us: u64,
}

#[derive(Debug, Clone)]
pub struct IdleResult {
    pub idle_ms: u64,
    pub samples: Vec<IdleSample>,
    pub total_latency_us: u64,
}

pub struct IdleDetector;

impl IdleDetector {
    pub fn idle_ms() -> Result<IdleResult, IdleError> {
        let t0 = Instant::now();

        let xi = Self::timed("xprintidle", Self::via_xprintidle);
        let mu = Self::timed("gnome-mutter", Self::via_gnome_mutter);
        let lc = Self::timed("loginctl", Self::via_loginctl);

        let mut samples = Vec::new();
        let mut best: Option<u64> = None;

        if let Some(s) = xi {
            best = best.map(|b| b.max(s.idle_ms)).or(Some(s.idle_ms));
            samples.push(s);
        }
        if let Some(s) = mu {
            best = best.map(|b| b.max(s.idle_ms)).or(Some(s.idle_ms));
            samples.push(s);
        }
        if let Some(s) = lc {
            best = best.map(|b| b.max(s.idle_ms)).or(Some(s.idle_ms));
            samples.push(s);
        }

        let total_latency = t0.elapsed().as_micros() as u64;

        best.map(|idle_ms| IdleResult { idle_ms, samples, total_latency_us: total_latency })
            .ok_or(IdleError::NoMethod)
    }

    fn timed(
        name: &'static str,
        f: fn() -> Result<u64, IdleError>,
    ) -> Option<IdleSample> {
        let t0 = Instant::now();
        match f() {
            Ok(idle_ms) => Some(IdleSample {
                idle_ms,
                method: name,
                latency_us: t0.elapsed().as_micros() as u64,
            }),
            Err(_) => None,
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

    fn via_loginctl() -> Result<u64, IdleError> {
        let output = Command::new("loginctl")
            .args(["list-sessions", "--no-legend"])
            .output()
            .map_err(|e| IdleError::Subprocess { source: e })?;

        if !output.status.success() {
            return Err(IdleError::NoMethod);
        }

        let text = String::from_utf8_lossy(&output.stdout);

        let session_id = text.lines().next()
            .and_then(|line| line.split_whitespace().next())
            .map(|s| s.to_string());

        let sid = match session_id {
            Some(id) if !id.is_empty() => id,
            _ => return Err(IdleError::NoMethod),
        };

        let output = Command::new("loginctl")
            .args(["show-session", &sid, "--property=IdleSinceHint"])
            .output()
            .map_err(|e| IdleError::Subprocess { source: e })?;

        if !output.status.success() {
            return Err(IdleError::NoMethod);
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.trim();

        if let Some(val) = line.strip_prefix("IdleSinceHint=") {
            let val = val.trim();
            if val.is_empty() || val == "0" {
                return Ok(0);
            }
            if let Ok(micros) = val.parse::<u64>() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;
                let idle_us = now.saturating_sub(micros);
                return Ok(idle_us / 1000);
            }
        }

        Err(IdleError::Parse)
    }
}
