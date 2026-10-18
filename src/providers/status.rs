//! Claude service status from a Statuspage `summary.json` (reusable for other Statuspage sites).

use crate::config::Config;
use crate::model::*;
use chrono::Utc;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread;
use std::time::Duration;

pub const CLAUDE_SUMMARY_URL: &str = "https://status.claude.com/api/v2/summary.json";
pub const OPENAI_SUMMARY_URL: &str = "https://status.openai.com/api/v2/summary.json";

/// OpenAI's page covers ChatGPT, the API, Sora…: only these components
/// (and incidents touching them or naming Codex) count for Codex.
pub fn codex_relevant(component: &str) -> bool {
    let c = component.to_lowercase();
    c.contains("codex") || c == "responses" || c == "login"
}

/// Like `parse_summary`, but judged only on components where `relevant`
/// holds; the page-wide indicator is ignored.
pub fn parse_summary_for(json: &str, relevant: fn(&str) -> bool) -> Option<(StatusLevel, String)> {
    let v: Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    v.get("status")?.get("indicator")?.as_str()?;
    let empty = Vec::new();
    let name = |x: &Value| x.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let statuses: Vec<String> = v
        .get("components")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|c| c.get("group").and_then(Value::as_bool) != Some(true) && relevant(&name(c)))
        .map(|c| c.get("status").and_then(Value::as_str).unwrap_or("operational").to_string())
        .collect();
    let level = if statuses.iter().any(|s| s == "major_outage") {
        StatusLevel::Red
    } else if statuses.iter().any(|s| s != "operational") {
        StatusLevel::Yellow
    } else {
        StatusLevel::Green
    };
    let names: Vec<String> = v
        .get("incidents")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|i| !matches!(i.get("status").and_then(Value::as_str), Some("resolved" | "postmortem")))
        .filter(|i| {
            name(i).to_lowercase().contains("codex")
                || i.get("components").and_then(Value::as_array).is_some_and(|cs| cs.iter().any(|c| relevant(&name(c))))
        })
        .map(name)
        .collect();
    // A relevant incident while every component still reads operational is degraded.
    let level = if level == StatusLevel::Green && !names.is_empty() { StatusLevel::Yellow } else { level };
    let text = match level {
        StatusLevel::Green => String::new(),
        _ if !names.is_empty() => names.join("; "),
        _ if statuses.iter().any(|s| s == "under_maintenance") => "maintenance".into(),
        _ => "some issues".into(),
    };
    Some((level, text))
}

fn fetch_body(url: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(concat!("majihanaya/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    resp.body_mut().read_to_string().map_err(|e| e.to_string())
}

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
    parse_summary(&fetch_body(url)?).ok_or_else(|| "unexpected status response".to_string())
}

fn apply(slot: &mut ServiceStatus, result: Result<(StatusLevel, String), String>) {
    match result {
        Ok((level, text)) => *slot = ServiceStatus { level, text, last_success: Some(Utc::now()), error: None },
        Err(e) => slot.error = Some(e),
    }
}

pub fn refresh(state: &Shared, _config: &Config) {
    let claude = fetch_summary(CLAUDE_SUMMARY_URL);
    let codex = fetch_body(OPENAI_SUMMARY_URL)
        .and_then(|b| parse_summary_for(&b, codex_relevant).ok_or_else(|| "unexpected status response".to_string()));
    let mut s = lock(state);
    apply(&mut s.claude_status, claude);
    apply(&mut s.codex_status, codex);
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
            crate::model::REFRESH.sleep(Duration::from_secs(config.status_interval_seconds.max(1)));
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
    fn codex_scope_ignores_unrelated_openai_components() {
        let page = |sora: &str, codex: &str, incident: &str| {
            format!(
                r#"{{"status":{{"indicator":"major"}},"components":[{{"name":"Sora","status":"{sora}"}},{{"name":"Codex in ChatGPT Desktop","status":"{codex}"}},{{"name":"Responses","status":"operational"}}],"incidents":[{incident}]}}"#
            )
        };
        // Sora down, Codex fine: green despite the page-wide "major".
        assert_eq!(parse_summary_for(&page("major_outage", "operational", ""), codex_relevant), Some((StatusLevel::Green, String::new())));
        assert_eq!(parse_summary_for(&page("operational", "major_outage", ""), codex_relevant).unwrap().0, StatusLevel::Red);
        let inc = r#"{"name":"Elevated Codex errors","status":"investigating","components":[]}"#;
        assert_eq!(
            parse_summary_for(&page("operational", "operational", inc), codex_relevant),
            Some((StatusLevel::Yellow, "Elevated Codex errors".into()))
        );
        let other = r#"{"name":"Sora slow","status":"investigating","components":[{"name":"Sora"}]}"#;
        assert_eq!(parse_summary_for(&page("degraded_performance", "operational", other), codex_relevant).unwrap().0, StatusLevel::Green);
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
