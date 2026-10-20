//! Claude Code version: local (`claude --version`) and latest for the update channel.

use crate::config::Config;
use crate::model::*;
use crate::proc;
use chrono::Utc;
use regex::Regex;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

const RELEASES_URL: &str = "https://downloads.claude.ai/claude-code-releases/";

fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d+\.\d+\.\d+)").expect("valid regex"))
}

pub fn parse_version(text: &str) -> Option<String> {
    version_re().captures(text).map(|c| c[1].to_string())
}

/// `autoUpdatesChannel` from settings.json: `stable` or `latest` (default).
pub fn parse_channel(settings_json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(settings_json.trim_start_matches('\u{feff}')).unwrap_or_default();
    match v.get("autoUpdatesChannel").and_then(|c| c.as_str()) {
        Some("stable") => "stable".to_string(),
        _ => "latest".to_string(),
    }
}

fn read_channel() -> String {
    let text = dirs::home_dir()
        .map(|h| h.join(".claude").join("settings.json"))
        .and_then(|p| proc::read_text_capped(&p, 1024 * 1024).ok())
        .unwrap_or_default();
    parse_channel(&text)
}

fn local_version(state: &Shared) -> Option<String> {
    let path = super::claude::resolve_cli()?;
    let out = proc::run_capture(state, &path, &["--version"], Duration::from_secs(15)).ok()?;
    parse_version(&out.stdout)
}

fn fetch_latest(channel: &str) -> Result<String, String> {
    let mut resp = super::status::http_agent().get(format!("{RELEASES_URL}{channel}")).call().map_err(|e| e.to_string())?;
    let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let body = body.trim();
    parse_version(body).filter(|v| v == body).ok_or_else(|| "unexpected version response".to_string())
}

pub fn refresh(state: &Shared, _config: &Config) {
    let channel = read_channel();
    let local = local_version(state);
    let latest = fetch_latest(&channel).ok();
    {
        let mut s = lock(state);
        if local.is_some() {
            s.version.local = local;
        }
        s.version.channel = Some(channel);
        if let Some(l) = latest {
            s.version.latest = Some(l);
            s.version.last_success = Some(Utc::now());
        }
    }
}

pub fn spawn(state: Shared, notify: Notify) {
    thread::spawn(move || {
        loop {
            // Read every cycle: Settings and config-file edits apply live.
            let config = crate::config::current();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                refresh(&state, &config);
                notify();
            }));
            crate::model::REFRESH.sleep(Duration::from_secs(config.version_interval_seconds.max(1)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_output() {
        assert_eq!(parse_version("2.1.302 (Claude Code)").as_deref(), Some("2.1.302"));
        assert_eq!(parse_version("  2.1.302\n").as_deref(), Some("2.1.302"));
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(parse_version("hello"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn channel_parsing() {
        assert_eq!(parse_channel(r#"{"autoUpdatesChannel":"stable"}"#), "stable");
        assert_eq!(parse_channel(r#"{"autoUpdatesChannel":"latest"}"#), "latest");
        assert_eq!(parse_channel(r#"{"autoUpdatesChannel":"beta"}"#), "latest");
        assert_eq!(parse_channel(r#"{"autoUpdatesChannel":3}"#), "latest");
        assert_eq!(parse_channel(r#"{"other":1}"#), "latest");
        assert_eq!(parse_channel(""), "latest");
        assert_eq!(parse_channel("not json"), "latest");
    }

    #[test]
    #[ignore = "network"]
    fn latest_smoke() {
        let v = fetch_latest("latest").expect("fetch");
        println!("latest = {v}");
        assert!(parse_version(&v).is_some());
    }
}
