//! Codex provider: plan-limit windows and account from `codex app-server`
//! (JSON-RPC 2.0 over stdio, no tokens spent), with the newest rollout log as
//! a fallback. The app-server protocol is not a stable public API: any
//! unexpected shape degrades to "Codex: unavailable" instead of panicking.

use crate::config::Config;
use crate::model::*;
use crate::proc;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const ACCOUNT_INTERVAL: Duration = Duration::from_secs(600);
const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// How much of the end of a rollout file is scanned for a `token_count` event.
const ROLLOUT_TAIL_BYTES: u64 = 1024 * 1024;
/// Longest app-server line we keep; replies are a few KB.
const MAX_LINE_BYTES: u64 = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Pure parsing
// ---------------------------------------------------------------------------

/// "5-hour", "week", otherwise whole hours ("24 h"); short windows in minutes.
pub fn window_label(mins: Option<u32>) -> String {
    match mins {
        None => "window".into(),
        Some(300) => "5-hour".into(),
        Some(10080) => "week".into(),
        Some(m) if m < 60 => format!("{m} min"),
        Some(m) => format!("{} h", m.saturating_add(30) / 60),
    }
}

fn window_from(v: &Value, pct_key: &str, mins_key: &str, reset_key: &str) -> Option<CodexWindow> {
    let percent = v.get(pct_key)?.as_f64().filter(|p| p.is_finite())?.clamp(0.0, 100.0) as f32;
    let window_mins = v.get(mins_key).and_then(Value::as_u64).and_then(|m| u32::try_from(m).ok());
    let resets_at = v.get(reset_key).and_then(Value::as_i64).and_then(|s| DateTime::from_timestamp(s, 0));
    Some(CodexWindow { label: window_label(window_mins), bucket: None, window_mins, percent, resets_at })
}

/// One bucket (`RateLimitSnapshot`): a window per non-null `primary`/`secondary`.
fn snapshot_windows(snap: &Value) -> Option<Vec<CodexWindow>> {
    let limits = snap.as_object()?;
    if !limits.contains_key("primary") && !limits.contains_key("secondary") {
        return None;
    }
    // Present but unparseable → unknown shape: fail rather than show "no windows".
    ["primary", "secondary"]
        .iter()
        .filter_map(|k| limits.get(*k))
        .filter(|v| !v.is_null())
        .map(|v| window_from(v, "usedPercent", "windowDurationMins", "resetsAt"))
        .collect()
}

/// The `result` of `account/rateLimits/read` (or a rateLimits notification's
/// params): the main bucket's windows, labelled by duration, then any extra
/// bucket from `rateLimitsByLimitId` (e.g. a per-model quota), prefixed with
/// its name. Extra buckets that fail to parse are skipped, not fatal.
pub fn parse_rate_limits(result: &Value) -> Option<Vec<CodexWindow>> {
    let main = result.get("rateLimits")?;
    let mut windows = snapshot_windows(main)?;
    let main_id = main.get("limitId").and_then(Value::as_str);
    if let Some(buckets) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, snap) in buckets {
            if Some(id.as_str()) == main_id {
                continue;
            }
            let name = ["limitName", "normalModelSlug"]
                .iter()
                .find_map(|k| snap.get(*k).and_then(Value::as_str))
                .unwrap_or(id);
            for mut w in snapshot_windows(snap).unwrap_or_default() {
                w.label = format!("{name} {}", w.label);
                w.bucket = Some(id.clone());
                windows.push(w);
            }
        }
    }
    Some(windows)
}

/// The `result` of `account/read`. A null account gives `None`.
pub fn parse_account(result: &Value) -> Option<CodexAccount> {
    let acc = result.get("account")?.as_object()?;
    let text = |k: &str| acc.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(CodexAccount { kind: text("type"), email: text("email"), plan_type: text("planType") })
}

/// A rollout line holding a `token_count` event with rate limits.
pub fn parse_rollout_line(line: &str) -> Option<(DateTime<Utc>, Vec<CodexWindow>)> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "event_msg" {
        return None;
    }
    let payload = v.get("payload")?;
    if payload.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let limits = payload.get("rate_limits")?.as_object()?;
    let time = DateTime::parse_from_rfc3339(v.get("timestamp")?.as_str()?).ok()?.with_timezone(&Utc);
    let windows: Vec<CodexWindow> = ["primary", "secondary"]
        .iter()
        .filter_map(|k| limits.get(*k))
        .filter(|w| !w.is_null())
        .filter_map(|w| window_from(w, "used_percent", "window_minutes", "resets_at"))
        .collect();
    if windows.is_empty() { None } else { Some((time, windows)) }
}

/// Last usable `token_count` event in a chunk of rollout text.
fn last_rollout_snapshot(text: &str) -> Option<(DateTime<Utc>, Vec<CodexWindow>)> {
    text.lines().rev().filter(|l| l.contains("token_count")).find_map(parse_rollout_line)
}

// ---------------------------------------------------------------------------
// Reply bookkeeping (no process involved, so it is unit-testable)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Collected {
    server_version: Option<String>,
    results: HashMap<u64, Result<Value, String>>,
    /// Windows from the latest rateLimits notification, not yet applied.
    notified: Option<Vec<CodexWindow>>,
}

impl Collected {
    fn take(&mut self, id: u64) -> Option<Result<Value, String>> {
        self.results.remove(&id)
    }
}

/// `majihanaya/0.149.0 (Windows ...)` -> `0.149.0`.
fn version_from_user_agent(ua: &str) -> Option<String> {
    let rest = ua.split_once('/')?.1;
    let v = rest.split(' ').next()?.trim();
    if v.is_empty() { None } else { Some(v.to_owned()) }
}

/// Handles one line from the server: replies are stored by id, rateLimits
/// notifications are parsed, everything else is ignored.
fn handle_line(line: &str, col: &mut Collected) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    if let Some(method) = v.get("method").and_then(Value::as_str) {
        if v.get("id").is_none() && method.contains("rateLimits") {
            if let Some(w) = v.get("params").and_then(parse_rate_limits) {
                col.notified = Some(w);
            }
        }
        return;
    }
    let Some(id) = v.get("id").and_then(Value::as_u64) else { return };
    if let Some(result) = v.get("result") {
        if let Some(ver) = result.get("userAgent").and_then(Value::as_str).and_then(version_from_user_agent) {
            col.server_version = Some(ver);
        }
        // A limits reply supersedes any notification seen before it; only
        // later notifications are newer.
        if result.get("rateLimits").is_some() {
            col.notified = None;
        }
        col.results.insert(id, Ok(result.clone()));
    } else if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("error");
        col.results.insert(id, Err(msg.chars().take(80).collect()));
    }
}

#[cfg(test)]
fn apply_lines(text: &str, col: &mut Collected) {
    for line in text.lines() {
        handle_line(line, col);
    }
}

// ---------------------------------------------------------------------------
// CLI resolution
// ---------------------------------------------------------------------------

/// Native `codex.exe` if we can find it (npm ships one next to its shim),
/// else a standalone `codex.exe` on PATH, else the `.cmd` shim itself.
pub fn resolve_cli() -> Option<PathBuf> {
    let found = proc::find_on_path("codex")?;
    let is_cmd = found.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd"));
    if !is_cmd {
        return Some(found);
    }
    if let Some(dir) = found.parent() {
        let pkg = dir.join("node_modules").join("@openai").join("codex");
        let candidates = [
            pkg.join("node_modules")
                .join("@openai")
                .join("codex-win32-x64")
                .join("vendor")
                .join("x86_64-pc-windows-msvc")
                .join("bin")
                .join("codex.exe"),
            pkg.join("vendor").join("x86_64-pc-windows-msvc").join("codex").join("codex.exe"),
        ];
        if let Some(exe) = candidates.into_iter().find(|p| p.is_file()) {
            return Some(exe);
        }
    }
    Some(found)
}

// ---------------------------------------------------------------------------
// Rollout fallback
// ---------------------------------------------------------------------------

fn sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    Some(home.join("sessions"))
}

fn collect_rollouts(dir: &Path, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect_rollouts(&path, out);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                if let Ok(mtime) = meta.modified() {
                    out.push((mtime, path));
                }
            }
        }
    }
}

fn read_tail(path: &Path) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(ROLLOUT_TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        // Drop the partial first line.
        return Some(text.split_once('\n').map(|(_, rest)| rest.to_owned()).unwrap_or_default());
    }
    Some(text)
}

/// Newest usable `token_count` event across the most recently modified
/// rollouts: a fresh session may not have logged one yet, and mtime order is
/// only a hint, so the event timestamps decide.
fn rollout_snapshot() -> Option<(DateTime<Utc>, Vec<CodexWindow>)> {
    const CANDIDATES: usize = 5;
    let mut files = Vec::new();
    collect_rollouts(&sessions_root()?, &mut files);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .iter()
        .take(CANDIDATES)
        .filter_map(|(_, p)| last_rollout_snapshot(&read_tail(p)?))
        .max_by_key(|(t, _)| *t)
}

// ---------------------------------------------------------------------------
// State writes
// ---------------------------------------------------------------------------

fn begin_attempt(state: &Shared) {
    lock(state).codex.last_attempt = Some(Utc::now());
}

/// Notifications carry only the main bucket: replace its windows, keep the
/// extra buckets' windows from the last full read.
fn merge_main(current: &[CodexWindow], main: Vec<CodexWindow>) -> Vec<CodexWindow> {
    let mut out: Vec<CodexWindow> = main.into_iter().filter(|w| w.bucket.is_none()).collect();
    out.extend(current.iter().filter(|w| w.bucket.is_some()).cloned());
    out
}

fn write_windows(state: &Shared, windows: Vec<CodexWindow>) {
    let mut s = lock(state);
    s.codex.presence = Presence::Found;
    s.codex.windows = windows;
    s.codex.source = CodexSource::AppServer;
    s.codex.source_time = None;
    s.codex.last_success = Some(Utc::now());
    s.codex.error = None;
}

fn write_failure(state: &Shared, msg: &str) {
    let mut s = lock(state);
    s.codex.error = Some(msg.to_owned());
}

/// App-server failed: use the newest rollout event if there is one, else
/// record the error (keeping previous windows).
fn fall_back(state: &Shared, err: &str) {
    if lock(state).codex.presence == Presence::Missing {
        write_failure(state, "Codex: not installed");
        return;
    }
    match rollout_snapshot() {
        Some((time, windows)) => {
            let mut s = lock(state);
            s.codex.presence = Presence::Found;
            s.codex.windows = windows;
            s.codex.source = CodexSource::Rollout;
            s.codex.source_time = Some(time);
            // The data is as old as the log entry, so staleness follows it.
            s.codex.last_success = Some(time);
            s.codex.error = None;
        }
        None => write_failure(state, &format!("Codex: unavailable ({err})")),
    }
}

// ---------------------------------------------------------------------------
// App-server session
// ---------------------------------------------------------------------------

struct Server {
    child: Child,
    /// Dropped after the kill in `Drop`: ends anything the child started.
    _job: Option<proc::JobGuard>,
    stdin: ChildStdin,
    rx: Receiver<String>,
    pid: u32,
    state: Shared,
    next_id: u64,
    col: Collected,
}

impl Server {
    fn start(state: &Shared, cli: &Path) -> Result<Self, String> {
        let mut child = proc::command(cli)
            .arg("app-server")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start codex: {}", e.kind()))?;
        let job = proc::adopt(&child);
        let pid = child.id();
        lock(state).own_pids.insert(pid);
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            lock(state).own_pids.remove(&pid);
            return Err("no stdio".into());
        };
        // Bounded: if we fall behind, the child waits on its pipe instead of
        // our memory growing.
        let (tx, rx) = mpsc::sync_channel(256);
        // "Refresh now" wakes `idle` with an empty line (ignored as input).
        let wake = tx.clone();
        REFRESH.add_hook(Box::new(move || !matches!(wake.try_send(String::new()), Err(mpsc::TrySendError::Disconnected(_)))));
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                // An over-long line arrives in pieces that fail to parse and are skipped.
                match (&mut reader).take(MAX_LINE_BYTES).read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        // Drain and discard stderr so the child never blocks on a full pipe.
        thread::spawn(move || {
            let _ = std::io::copy(&mut BufReader::new(stderr), &mut std::io::sink());
        });
        Ok(Self { child, _job: job, stdin, rx, pid, state: state.clone(), next_id: 2, col: Collected::default() })
    }

    fn send(&mut self, msg: Value) -> Result<(), String> {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).and_then(|_| self.stdin.flush()).map_err(|_| "codex exited".to_string())
    }

    /// Reads lines until every id has a reply; unrelated lines are handled as usual.
    fn wait_for(&mut self, ids: &[u64]) -> Result<(), String> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        while !ids.iter().all(|id| self.col.results.contains_key(id)) {
            // Checked first: a stream of unrelated lines must not outlast the deadline.
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("timeout".into());
            }
            match self.rx.recv_timeout(left) {
                Ok(line) => handle_line(&line, &mut self.col),
                Err(RecvTimeoutError::Timeout) => return Err("timeout".into()),
                Err(RecvTimeoutError::Disconnected) => return Err("codex exited".into()),
            }
        }
        Ok(())
    }

    fn handshake(&mut self) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"clientInfo":{"name":"majihanaya","version":env!("CARGO_PKG_VERSION")}}}))?;
        self.wait_for(&[1])?;
        if let Some(Err(e)) = self.col.take(1) {
            return Err(e);
        }
        self.send(json!({"jsonrpc":"2.0","method":"initialized"}))
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({"jsonrpc":"2.0","id":id,"method":method});
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.send(msg)?;
        Ok(id)
    }

    /// One poll: limits (and the account when asked). Writes the state.
    fn poll(&mut self, want_account: bool) -> Result<(), String> {
        begin_attempt(&self.state);
        self.col.notified = None;
        // Polls skip the reset-credit detail lookup (the schema's advice for
        // background polls); the reply still carries the available count.
        let limits_id = self.request("account/rateLimits/read", Some(json!({"excludeResetCreditDetails": true})))?;
        let account_id = if want_account { Some(self.request("account/read", Some(json!({})))?) } else { None };
        let mut ids = vec![limits_id];
        ids.extend(account_id);
        self.wait_for(&ids)?;

        // The account first, even if the limits failed: a logged-out answer
        // (`account: null`) must replace the old identity.
        let account = account_id.and_then(|id| self.col.take(id)).and_then(Result::ok).map(|r| parse_account(&r));
        {
            let mut s = lock(&self.state);
            s.codex.server_version = self.col.server_version.clone();
            if let Some(acc) = account {
                s.codex.account = acc;
            }
        }
        let limits = self.col.take(limits_id).unwrap_or_else(|| Err("no reply".into()));
        let windows = match limits {
            Ok(v) => parse_rate_limits(&v).ok_or_else(|| "unexpected response".to_string())?,
            Err(e) => return Err(e),
        };
        // A rateLimits notification that arrived after the reply is newer
        // (for the main bucket; extra buckets keep the reply's values).
        let windows = match self.col.notified.take() {
            Some(main) => merge_main(&windows, main),
            None => windows,
        };
        write_windows(&self.state, windows);
        Ok(())
    }

    /// Waits up to `dur`, applying rateLimits notifications as they arrive.
    /// Errors when the child's stdout closes.
    fn idle(&mut self, dur: Duration, notify: &Notify) -> Result<(), String> {
        let deadline = Instant::now() + dur;
        let epoch = REFRESH.epoch();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || REFRESH.epoch() != epoch {
                return Ok(());
            }
            match self.rx.recv_timeout(left) {
                Ok(line) => {
                    handle_line(&line, &mut self.col);
                    if let Some(main) = self.col.notified.take() {
                        let current = lock(&self.state).codex.windows.clone();
                        write_windows(&self.state, merge_main(&current, main));
                        notify();
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => return Err("codex exited".into()),
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        lock(&self.state).own_pids.remove(&self.pid);
    }
}

fn start_server(state: &Shared) -> Result<Server, String> {
    let Some(cli) = resolve_cli() else {
        lock(state).codex.presence = Presence::Missing;
        return Err("codex not found".into());
    };
    lock(state).codex.presence = Presence::Found;
    let mut server = Server::start(state, &cli)?;
    server.handshake()?;
    Ok(server)
}

/// One-shot update: start app-server, read limits + account, stop it. Falls
/// back to the newest rollout log when the app-server cannot be used.
pub fn refresh(state: &Shared, _config: &Config) {
    begin_attempt(state);
    if let Err(e) = start_server(state).and_then(|mut s| s.poll(true)) {
        fall_back(state, &e);
    }
}

/// Worker: one long-lived app-server child polled every `codex_interval_seconds`.
pub fn spawn(state: Shared, config: Config, notify: Notify) {
    let _ = thread::Builder::new().name("codex".into()).spawn(move || {
        let interval = Duration::from_secs(config.codex_interval_seconds.max(5));
        let mut failures: u32 = 0;
        loop {
            let ran_ok = catch_unwind(AssertUnwindSafe(|| run_session(&state, interval, &notify))).unwrap_or(false);
            if ran_ok {
                failures = 0;
            }
            failures = failures.saturating_add(1);
            let backoff = interval.saturating_mul(1u32 << (failures - 1).min(10)).min(MAX_BACKOFF);
            REFRESH.sleep(backoff);
        }
    });
}

/// Runs one child until it fails. Returns whether at least one poll succeeded.
fn run_session(state: &Shared, interval: Duration, notify: &Notify) -> bool {
    let mut ok = false;
    let mut last_account: Option<Instant> = None;
    let result: Result<(), String> = (|| {
        begin_attempt(state);
        let mut server = start_server(state)?;
        loop {
            let want_account = last_account.is_none_or(|t| t.elapsed() >= ACCOUNT_INTERVAL);
            server.poll(want_account)?;
            if want_account {
                last_account = Some(Instant::now());
            }
            ok = true;
            notify();
            server.idle(interval, notify)?;
        }
    })();
    if let Err(e) = result {
        fall_back(state, &e);
        notify();
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUS: &str = include_str!("../../tests/fixtures/codex_appserver_plus.jsonl");
    const TWO: &str = include_str!("../../tests/fixtures/codex_ratelimits_two_windows.json");
    const ROLLOUT: &str = include_str!("../../tests/fixtures/codex_rollout_token_count.jsonl");

    #[test]
    fn plus_fixture_matches_by_id_and_skips_notification() {
        let mut col = Collected::default();
        // Replies arrive out of order (3 before 2) with a notification between.
        apply_lines(PLUS, &mut col);
        assert_eq!(col.server_version.as_deref(), Some("0.149.0"));
        assert!(col.notified.is_none(), "remoteControl notification must be ignored");
        let windows = parse_rate_limits(&col.take(2).unwrap().unwrap()).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "week");
        assert_eq!(windows[0].percent, 0.0);
        assert_eq!(windows[0].window_mins, Some(10080));
        assert_eq!(windows[0].resets_at.unwrap().timestamp(), 1792918611);
        let acc = parse_account(&col.take(3).unwrap().unwrap()).unwrap();
        assert_eq!(acc.kind.as_deref(), Some("chatgpt"));
        assert_eq!(acc.plan_type.as_deref(), Some("plus"));
        assert_eq!(acc.email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn crlf_lines_are_handled() {
        let mut col = Collected::default();
        apply_lines(&PLUS.replace('\n', "\r\n"), &mut col);
        assert!(col.take(2).is_some() && col.take(3).is_some());
    }

    #[test]
    fn two_windows_fixture() {
        let v: Value = serde_json::from_str(TWO).unwrap();
        let w = parse_rate_limits(&v["result"]).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].label.as_str(), w[0].percent), ("5-hour", 42.5));
        assert_eq!((w[1].label.as_str(), w[1].percent), ("week", 63.0));
    }

    #[test]
    fn labels_follow_duration_not_slot() {
        let v = json!({"rateLimits":{"primary":{"usedPercent":1,"windowDurationMins":10080,"resetsAt":1},"secondary":{"usedPercent":2,"windowDurationMins":300,"resetsAt":2}}});
        let w = parse_rate_limits(&v).unwrap();
        assert_eq!((w[0].label.as_str(), w[1].label.as_str()), ("week", "5-hour"));
    }

    #[test]
    fn rate_limit_notification_is_applied() {
        let mut col = Collected::default();
        let line = r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":5},"secondary":null}}}"#;
        handle_line(line, &mut col);
        let w = col.notified.unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].label, "5-hour");
    }

    #[test]
    fn rollout_fixture() {
        let (time, w) = last_rollout_snapshot(ROLLOUT).unwrap();
        assert_eq!(time, DateTime::parse_from_rfc3339("2026-10-17T09:15:44.5Z").unwrap().with_timezone(&Utc));
        assert_eq!(w.len(), 1);
        assert_eq!((w[0].label.as_str(), w[0].percent), ("week", 12.0));
        assert_eq!(w[0].resets_at.unwrap().timestamp(), 1792918611);
        assert!(parse_rollout_line(ROLLOUT.lines().next().unwrap()).is_none());
    }

    #[test]
    fn labels() {
        assert_eq!(window_label(Some(300)), "5-hour");
        assert_eq!(window_label(Some(10080)), "week");
        assert_eq!(window_label(Some(1440)), "24 h");
        assert_eq!(window_label(Some(120)), "2 h");
        assert_eq!(window_label(Some(90)), "2 h");
        assert_eq!(window_label(Some(45)), "45 min");
        assert_eq!(window_label(None), "window");
    }

    #[test]
    fn malformed_and_unknown_shapes_give_none() {
        assert!(parse_rollout_line("not json").is_none());
        assert!(parse_rollout_line("{}").is_none());
        assert!(parse_rollout_line(r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":null}}"#).is_none());
        for v in [json!(null), json!([]), json!({}), json!({"rateLimits":null}), json!({"rateLimits":5})] {
            assert!(parse_rate_limits(&v).is_none());
            assert!(parse_account(&v).is_none());
        }
        assert!(parse_account(&json!({"account":null})).is_none());
        // A present but unparseable window is an unknown shape, not "no windows".
        assert!(parse_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":"x"}}})).is_none());
        assert!(parse_rate_limits(&json!({"rateLimits":{"other":1}})).is_none());
        // Both windows explicitly null is a real, empty answer.
        assert_eq!(parse_rate_limits(&json!({"rateLimits":{"primary":null,"secondary":null}})).unwrap().len(), 0);
        let mut col = Collected::default();
        apply_lines("garbage\n{\"id\":\"x\"}\n[1]\n\n{\"id\":9,\"error\":{\"message\":\"no auth\"}}", &mut col);
        assert_eq!(col.take(9), Some(Err("no auth".into())));
    }

    #[test]
    fn notification_order_against_limits_reply() {
        let note = |p: u32| {
            format!(r#"{{"method":"account/rateLimits/updated","params":{{"rateLimits":{{"primary":{{"usedPercent":{p},"windowDurationMins":10080}}}}}}}}"#)
        };
        let reply = r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":10,"windowDurationMins":10080}}}}"#;
        // Notification before the reply: superseded by it.
        let mut col = Collected::default();
        apply_lines(&format!("{}\n{reply}", note(5)), &mut col);
        assert!(col.notified.is_none());
        // Notification after the reply: newer, kept.
        let mut col = Collected::default();
        apply_lines(&format!("{reply}\n{}", note(20)), &mut col);
        assert_eq!(col.notified.unwrap()[0].percent, 20.0);
    }

    #[test]
    fn extra_buckets_are_named_and_survive_notifications() {
        let reply = json!({
            "rateLimits": {"limitId": "codex", "primary": {"usedPercent": 2, "windowDurationMins": 10080}, "secondary": null},
            "rateLimitsByLimitId": {
                "codex": {"limitId": "codex", "primary": {"usedPercent": 2, "windowDurationMins": 10080}},
                "luna": {"limitId": "luna", "limitName": "Luna", "primary": {"usedPercent": 40, "windowDurationMins": 300}},
                "broken": {"limitId": "broken", "primary": {"usedPercent": "x"}}
            }
        });
        let w = parse_rate_limits(&reply).unwrap();
        let labels: Vec<_> = w.iter().map(|w| (w.label.as_str(), w.bucket.as_deref())).collect();
        assert_eq!(labels, [("week", None), ("Luna 5-hour", Some("luna"))]);
        // A notification (main bucket only) keeps the extra bucket.
        let note = parse_rate_limits(&json!({"rateLimits": {"primary": {"usedPercent": 9, "windowDurationMins": 10080}}})).unwrap();
        let merged = merge_main(&w, note);
        assert_eq!((merged[0].percent, merged[1].label.as_str()), (9.0, "Luna 5-hour"));
    }

    #[test]
    fn extreme_numbers_are_bounded() {
        assert_eq!(window_label(Some(u32::MAX)), format!("{} h", u32::MAX / 60));
        let w = parse_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":1e100,"windowDurationMins":300}}}))
            .unwrap();
        assert_eq!(w[0].percent, 100.0);
    }

    #[test]
    fn user_agent_version() {
        assert_eq!(version_from_user_agent("majihanaya/0.149.0 (Windows)").as_deref(), Some("0.149.0"));
        assert_eq!(version_from_user_agent("nothing"), None);
    }

    /// Real installed codex-cli: `cargo test providers::codex -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn smoke_real_codex() {
        let state: Shared = Default::default();
        refresh(&state, &Config::default());
        let s = lock(&state);
        let c = &s.codex;
        let email = c.account.as_ref().and_then(|a| a.email.as_deref()).map(|e| {
            let (user, domain) = e.split_once('@').unwrap_or((e, ""));
            format!("{}***@{}", user.chars().next().unwrap_or('?'), domain)
        });
        println!(
            "presence={:?} source={:?} version={:?} account={:?}/{:?}/{:?} windows={:?} error={:?}",
            c.presence,
            c.source,
            c.server_version,
            c.account.as_ref().and_then(|a| a.kind.clone()),
            email,
            c.account.as_ref().and_then(|a| a.plan_type.clone()),
            c.windows.iter().map(|w| format!("{} {}% reset {:?}", w.label, w.percent, w.resets_at)).collect::<Vec<_>>(),
            c.error,
        );
        assert_eq!(c.presence, Presence::Found);
        assert_eq!(c.source, CodexSource::AppServer);
        assert!(s.own_pids.is_empty());
    }
}
