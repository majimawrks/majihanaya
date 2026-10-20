//! Claude Code sessions from `~/.claude/sessions/<pid>.json`.
//! Only `*.json` files are read; the folder also holds `*.key` files that are never opened.

use crate::config::Config;
use crate::model::*;
use crate::proc;
use chrono::Utc;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Start-time tolerance: 5 s in FILETIME ticks (100 ns).
const TOLERANCE_TICKS: u64 = 50_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct RawSession {
    pub pid: u32,
    pub proc_start: u64,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub waiting_for: Option<String>,
    pub entrypoint: Option<String>,
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn parse_session(json: &str) -> Option<RawSession> {
    let v: Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    let pid = u32::try_from(v.get("pid")?.as_u64()?).ok()?;
    let proc_start = match v.get("procStart")? {
        Value::String(s) => s.trim().parse().ok()?,
        Value::Number(n) => n.as_u64()?,
        _ => return None,
    };
    Some(RawSession {
        pid,
        proc_start,
        cwd: str_field(&v, "cwd"),
        name: str_field(&v, "name"),
        status: str_field(&v, "status"),
        waiting_for: str_field(&v, "waitingFor"),
        entrypoint: str_field(&v, "entrypoint"),
    })
}

pub fn is_alive_match(proc_start: u64, recorded: u64) -> bool {
    proc_start.abs_diff(recorded) <= TOLERANCE_TICKS
}

fn display_name(raw: &RawSession) -> String {
    if let Some(n) = &raw.name {
        return n.clone();
    }
    raw.cwd
        .as_deref()
        .and_then(|c| c.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "session".to_string())
}

fn to_session(raw: &RawSession) -> Session {
    let status = match raw.status.as_deref() {
        Some("busy") => SessionStatus::Working,
        Some("waiting") => SessionStatus::Waiting(raw.waiting_for.clone()),
        _ => SessionStatus::Idle,
    };
    Session {
        pid: raw.pid,
        name: display_name(raw),
        status,
        cwd: raw.cwd.clone(),
        entrypoint: raw.entrypoint.clone(),
    }
}

fn rank(s: &SessionStatus) -> u8 {
    match s {
        SessionStatus::Waiting(_) => 0,
        SessionStatus::Working => 1,
        SessionStatus::Idle => 2,
    }
}

fn sort_sessions(list: &mut [Session]) {
    list.sort_by(|a, b| {
        rank(&a.status)
            .cmp(&rank(&b.status))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

const MAX_SESSION_FILES: usize = 512;

fn sessions_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("sessions"))
}

/// `None` when the folder exists but can't be read: keep the previous list.
fn read_all(state: &Shared) -> Option<Vec<Session>> {
    let Some(dir) = sessions_dir() else { return Some(Vec::new()) };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let own = lock(state).own_pids.clone();
    let mut list: Vec<Session> = Vec::new();
    // Far more files than sessions anyone runs: a flooded folder costs a
    // bounded amount of work and memory.
    for entry in entries.flatten().take(MAX_SESSION_FILES) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // A session file is a few hundred bytes; skip anything absurd.
        let Ok(text) = proc::read_text_capped(&path, 64 * 1024) else { continue };
        let Some(raw) = parse_session(&text) else { continue };
        // One row per process: more files naming the same PID add nothing.
        if own.contains(&raw.pid) || list.iter().any(|s| s.pid == raw.pid) {
            continue;
        }
        if let Some(start) = proc::process_start_filetime(raw.pid) {
            if is_alive_match(start, raw.proc_start) {
                list.push(to_session(&raw));
            }
        }
    }
    sort_sessions(&mut list);
    Some(list)
}

pub fn refresh(state: &Shared, _config: &Config) {
    if let Some(list) = read_all(state) {
        lock(state).sessions = SessionsState { list, last_success: Some(Utc::now()) };
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
            crate::model::REFRESH.sleep(Duration::from_secs(config.sessions_interval_seconds.max(1)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"pid":4242,"procStart":"134367851097891183","cwd":"C:\\Users\\me\\Lab\\proj","name":"my task","status":"waiting","waitingFor":"approve Bash","kind":"interactive","sessionId":"abc","entrypoint":"claude-desktop","extra":{"x":1}}"#;

    fn raw(status: Option<&str>, name: Option<&str>, cwd: Option<&str>) -> RawSession {
        RawSession {
            pid: 1,
            proc_start: 0,
            cwd: cwd.map(Into::into),
            name: name.map(Into::into),
            status: status.map(Into::into),
            waiting_for: Some("input".into()),
            entrypoint: None,
        }
    }

    #[test]
    fn parses_sample() {
        let r = parse_session(SAMPLE).unwrap();
        assert_eq!(r.pid, 4242);
        assert_eq!(r.proc_start, 134367851097891183);
        assert_eq!(r.name.as_deref(), Some("my task"));
        assert_eq!(r.waiting_for.as_deref(), Some("approve Bash"));
        assert_eq!(r.entrypoint.as_deref(), Some("claude-desktop"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_session("not json").is_none());
        assert!(parse_session(r#"{"procStart":"1"}"#).is_none());
        assert!(parse_session(r#"{"pid":1,"procStart":"x"}"#).is_none());
    }

    #[test]
    fn alive_tolerance() {
        let t = 134367851097891183u64;
        assert!(is_alive_match(t, t));
        assert!(is_alive_match(t + 50_000_000, t));
        assert!(is_alive_match(t - 50_000_000, t));
        assert!(!is_alive_match(t + 50_000_001, t));
        assert!(!is_alive_match(t - 50_000_001, t));
    }

    #[test]
    fn status_mapping() {
        assert_eq!(to_session(&raw(Some("busy"), Some("a"), None)).status, SessionStatus::Working);
        assert_eq!(
            to_session(&raw(Some("waiting"), Some("a"), None)).status,
            SessionStatus::Waiting(Some("input".into()))
        );
        assert_eq!(to_session(&raw(Some("idle"), Some("a"), None)).status, SessionStatus::Idle);
        assert_eq!(to_session(&raw(None, Some("a"), None)).status, SessionStatus::Idle);
    }

    #[test]
    fn name_fallback() {
        assert_eq!(display_name(&raw(None, Some("n"), Some("C:\\x\\y"))), "n");
        assert_eq!(display_name(&raw(None, None, Some("C:\\Users\\me\\proj"))), "proj");
        assert_eq!(display_name(&raw(None, None, Some("C:\\Users\\me\\proj\\"))), "proj");
        assert_eq!(display_name(&raw(None, None, Some("/home/me/other"))), "other");
        assert_eq!(display_name(&raw(None, None, None)), "session");
    }

    #[test]
    fn sorting() {
        let mk = |status: &str, name: &str| to_session(&raw(Some(status), Some(name), None));
        let mut list = vec![mk("idle", "a"), mk("busy", "z"), mk("waiting", "m"), mk("busy", "b"), mk("idle", "A0")];
        sort_sessions(&mut list);
        let names: Vec<_> = list.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["m", "b", "z", "a", "A0"]);
    }
}
