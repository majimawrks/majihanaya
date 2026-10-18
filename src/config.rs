//! `%APPDATA%\majihanaya\config.json`: load, save, unknown keys preserved.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io;
use std::path::PathBuf;

/// Which usage window the tray icon's number shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrayNumber {
    /// Highest % among all live windows (design default).
    #[default]
    Worst,
    ClaudeSession,
    ClaudeWeek,
    /// Worst Codex window.
    Codex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub usage_interval_seconds: u64,
    pub codex_interval_seconds: u64,
    pub account_interval_seconds: u64,
    pub status_interval_seconds: u64,
    pub version_interval_seconds: u64,
    pub sessions_interval_seconds: u64,
    pub warn_percent: u8,
    pub crit_percent: u8,
    pub show_claude: bool,
    pub show_codex: bool,
    pub show_sessions: bool,
    pub show_status: bool,
    pub show_account: bool,
    pub show_stats: bool,
    pub use_desktop_fallback: bool,
    pub scale: f32,
    pub opacity: f32,
    pub topmost: bool,
    pub expanded: bool,
    pub tray_number: TrayNumber,
    pub tray_reset_clock: bool,
    pub language: String,
    pub left: Option<f32>,
    pub top: Option<f32>,
    /// Keys this version does not know, written back unchanged.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            usage_interval_seconds: 60,
            codex_interval_seconds: 60,
            account_interval_seconds: 600,
            status_interval_seconds: 120,
            version_interval_seconds: 3600,
            sessions_interval_seconds: 10,
            warn_percent: 60,
            crit_percent: 85,
            show_claude: true,
            show_codex: true,
            show_sessions: true,
            show_status: true,
            show_account: true,
            show_stats: true,
            use_desktop_fallback: true,
            scale: 1.0,
            opacity: 1.0,
            topmost: false,
            expanded: false,
            tray_number: TrayNumber::Worst,
            tray_reset_clock: false,
            language: "en".into(),
            left: None,
            top: None,
            extra: Map::new(),
        }
    }
}

static CURRENT: std::sync::RwLock<Option<Config>> = std::sync::RwLock::new(None);

/// The live config: what workers read each cycle. `publish` replaces it.
pub fn current() -> Config {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub fn publish(config: &Config) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(config.clone());
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("majihanaya").join("config.json"))
}

impl Config {
    /// Missing file → defaults (and the file is created). Unreadable or
    /// invalid file → defaults, and the file is left alone so nothing is lost.
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Self>(text.trim_start_matches('\u{feff}')).unwrap_or_default().sanitized(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let cfg = Self::default();
                let _ = cfg.save();
                cfg
            }
            Err(_) => Self::default(),
        }
    }

    /// Clamps values a hand-edited file could make dangerous (zero or huge
    /// intervals). Applied on load, so the clamped values are what gets saved.
    pub fn sanitized(mut self) -> Self {
        const DAY: u64 = 86_400;
        for v in [
            &mut self.usage_interval_seconds,
            &mut self.codex_interval_seconds,
            &mut self.account_interval_seconds,
            &mut self.status_interval_seconds,
            &mut self.version_interval_seconds,
            &mut self.sessions_interval_seconds,
        ] {
            *v = (*v).clamp(5, DAY);
        }
        self.warn_percent = self.warn_percent.min(100);
        self.crit_percent = self.crit_percent.clamp(self.warn_percent, 100);
        self
    }

    /// Writes to a temp file then renames, so a crash never leaves half a file.
    pub fn save(&self) -> io::Result<()> {
        let path = path().ok_or_else(|| io::Error::other("no config dir"))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_round_trip() {
        let cfg: Config = serde_json::from_str(r#"{"warnPercent":50,"futureKey":{"a":1}}"#).unwrap();
        assert_eq!(cfg.warn_percent, 50);
        assert_eq!(cfg.crit_percent, 85);
        let out: Value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(out["futureKey"]["a"], 1);
        assert_eq!(out["warnPercent"], 50);
    }

    #[test]
    fn sanitize_clamps_intervals() {
        let cfg: Config =
            serde_json::from_str(r#"{"usageIntervalSeconds":0,"statusIntervalSeconds":18446744073709551615}"#).unwrap();
        let cfg = cfg.sanitized();
        assert_eq!(cfg.usage_interval_seconds, 5);
        assert_eq!(cfg.status_interval_seconds, 86_400);
    }

    #[test]
    fn empty_object_is_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, Config::default());
    }
}
