//! Claude service status from a Statuspage `summary.json` (reusable for other Statuspage sites).

use crate::config::Config;
use crate::model::*;
use chrono::Utc;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread;
use std::time::Duration;

pub const CLAUDE_SUMMARY_URL: &str = "https://status.claude.com/api/v2/summary.json";

/// Level and text of a Statuspage summary; `None` when the JSON is not one.
pub fn parse_summary(json: &str) -> Option<(StatusLevel, String)> {
    let v: Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    let indicator = v.get("status")?.get("indicator")?.as_str()?;
    let empty = Vec::new();
    let components = v.get("components").and_then(Value::as_array).unwrap_or(&empty);
    let leaf: Vec<&Value> =
        components.iter().filter(|c| c.get("group").and_then(Value::as_bool) != Some(true)).collect();
    let component_status = |c: &&Value| c.get("status").and_then(Value::as_str).unwrap_or("operational").to_string();
    let all_ok = leaf.iter().all(|c| component_status(c) == "operational");

    let level = match indicator {
        "none" if all_ok => StatusLevel::Green,
        "major" | "critical" => StatusLevel::Red,
        _ => StatusLevel::Yellow,
    };
    if level == StatusLevel::Green {
        return Some((level, String::new()));
    }

    let incidents = v.get("incidents").and_then(Value::as_array).unwrap_or(&empty);
    let names: Vec<&str> = incidents
        .iter()
        .filter(|i| !matches!(i.get("status").and_then(Value::as_str), Some("resolved" | "postmortem")))
        .filter_map(|i| i.get("name").and_then(Value::as_str))
        .collect();
    let text = if !names.is_empty() {
        names.join("; ")
    } else {
        let maint = v
            .get("scheduled_maintenances")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
            .iter()
            .any(|m| m.get("status").and_then(Value::as_str) == Some("in_progress"))
            || leaf.iter().any(|c| component_status(c) == "under_maintenance");
        if maint { "maintenance" } else { "some issues" }.to_string()
    };
    Some((level, text))
}

pub fn fetch_summary(url: &str) -> Result<(StatusLevel, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(concat!("majihanaya/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    parse_summary(&body).ok_or_else(|| "unexpected status response".to_string())
}

pub fn refresh(state: &Shared, _config: &Config) {
    let result = fetch_summary(CLAUDE_SUMMARY_URL);
    {
        let mut s = lock(state);
        match result {
            Ok((level, text)) => {
                s.claude_status = ServiceStatus { level, text, last_success: Some(Utc::now()), error: None };
            }
            Err(e) => s.claude_status.error = Some(e),
        }
    }
}

pub fn spawn(state: Shared, config: Config, notify: Notify) {
    let interval = Duration::from_secs(config.status_interval_seconds.max(1));
    thread::spawn(move || {
        loop {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                refresh(&state, &config);
                notify();
            }));
            crate::model::REFRESH.sleep(interval);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_ok() {
        let json = include_str!("../../tests/fixtures/status_summary_ok.json");
        assert_eq!(parse_summary(json), Some((StatusLevel::Green, String::new())));
    }

    #[test]
    fn fixture_incident() {
        let json = include_str!("../../tests/fixtures/status_summary_incident.json");
        assert_eq!(
            parse_summary(json),
            Some((StatusLevel::Yellow, "Elevated errors on Claude Code".to_string()))
        );
    }

    #[test]
    fn major_is_red() {
        let json = r#"{"status":{"indicator":"major"},"components":[],"incidents":[
            {"name":"A","status":"identified"},{"name":"Old","status":"resolved"},{"name":"B","status":"monitoring"}]}"#;
        assert_eq!(parse_summary(json), Some((StatusLevel::Red, "A; B".to_string())));
        let crit = r#"{"status":{"indicator":"critical"},"components":[],"incidents":[]}"#;
        assert_eq!(parse_summary(crit), Some((StatusLevel::Red, "some issues".to_string())));
    }

    #[test]
    fn component_under_maintenance() {
        let json = r#"{"status":{"indicator":"none"},"components":[
            {"name":"g","status":"under_maintenance","group":true},
            {"name":"api","status":"under_maintenance","group":false}],"incidents":[],"scheduled_maintenances":[]}"#;
        assert_eq!(parse_summary(json), Some((StatusLevel::Yellow, "maintenance".to_string())));
    }

    #[test]
    fn group_component_ignored() {
        let json = r#"{"status":{"indicator":"none"},"components":[
            {"name":"g","status":"degraded_performance","group":true},
            {"name":"api","status":"operational","group":false}]}"#;
        assert_eq!(parse_summary(json), Some((StatusLevel::Green, String::new())));
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse_summary("<html>").is_none());
        assert!(parse_summary("{}").is_none());
    }

    #[test]
    #[ignore = "network"]
    fn live_smoke() {
        let (level, text) = fetch_summary(CLAUDE_SUMMARY_URL).expect("fetch");
        println!("{level:?} {text:?}");
        assert_ne!(level, StatusLevel::Unknown);
    }
}
