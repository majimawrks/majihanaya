//! Codex sessions for CAM 04, best effort. Codex keeps no live registry
//! like Claude Code's `~/.claude/sessions`, so:
//!
//! - a Codex CLI session is a running `codex.exe` without a server
//!   subcommand, matched to the `codex-tui` rollout it started;
//! - ChatGPT Desktop runs every Codex conversation in one app-server it
//!   starts, so it gets one row: its most recently written conversation.
//!
//! Status comes from the rollout's last turn event: an open turn is RUN, a
//! completed one IDLE, anything else unknown. Within an open turn, a command
//! call that asked for escalated permissions (`sandbox_permissions:
//! "require_escalated"`) and has no output yet is waiting for the user's
//! approval: WAIT. Only the header, turn events and that flag are read;
//! prompts, commands and replies are never kept.

use crate::model::{Session, SessionStatus, Tool};
use crate::proc;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A rollout's header line carries the whole system prompt: keep it bounded.
const HEADER_MAX_BYTES: u64 = 1024 * 1024;
/// How much of a rollout's end is searched for its last turn event.
const TAIL_BYTES: u64 = 256 * 1024;
/// The title index only grows; its end holds the recent names.
const INDEX_TAIL_BYTES: u64 = 1024 * 1024;
/// A CLI writes its rollout header within seconds of starting.
const MATCH_WINDOW_SECS: i64 = 10 * 60;

pub const DESKTOP_ENTRYPOINT: &str = "codex-desktop";

/// Codex subcommands that are not an interactive session.
const NON_SESSION: [&str; 12] =
    ["app-server", "exec-server", "exec", "mcp-server", "mcp", "login", "logout", "apply", "sandbox", "completion", "cloud", "debug"];

/// Whether a `codex.exe` command line is an interactive session (no
/// subcommand, or `resume`), judged on the words after the executable.
pub fn is_interactive(cmdline: &str) -> bool {
    let rest = skip_program(cmdline);
    !rest.split_whitespace().any(|w| NON_SESSION.contains(&w.trim_matches('"')))
}

/// The command line without its first word (the executable, maybe quoted).
fn skip_program(cmdline: &str) -> &str {
    let s = cmdline.trim_start();
    let end = if let Some(q) = s.strip_prefix('"') {
        q.find('"').map_or(s.len(), |i| i + 2)
    } else {
        s.find(char::is_whitespace).unwrap_or(s.len())
    };
    &s[end..]
}

/// What a rollout's header line says about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub id: String,
    pub started: DateTime<Utc>,
    pub originator: String,
    pub cwd: Option<String>,
}

pub fn parse_header(line: &str) -> Option<Header> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let p = v.get("payload")?;
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(Header {
        id: s("id")?,
        started: DateTime::parse_from_rfc3339(&s("timestamp")?).ok()?.with_timezone(&Utc),
        originator: s("originator").unwrap_or_default(),
        cwd: s("cwd"),
    })
}

/// What waiting for an approval is called in the row.
pub const APPROVAL: &str = "permission";

/// RUN while the last turn event is a start (WAIT if that turn has an
/// escalated command call still without output), IDLE after a completion
/// or interruption, unknown when no turn event is in sight.
pub fn status_from_tail(text: &str) -> SessionStatus {
    let mut status = SessionStatus::Unknown;
    // Escalated calls of the current turn that have no output yet.
    let mut pending: HashSet<String> = HashSet::new();
    for line in text.lines() {
        // Cheap filter before parsing.
        let turn = line.contains("task_started") || line.contains("task_complete") || line.contains("turn_aborted");
        let call = line.contains("require_escalated") || line.contains("function_call_output");
        if !(turn || call) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(p) = v.get("payload") else { continue };
        let kind = p.get("type").and_then(Value::as_str);
        match (v.get("type").and_then(Value::as_str), kind) {
            (Some("event_msg"), Some("task_started")) => {
                status = SessionStatus::Running;
                pending.clear();
            }
            (Some("event_msg"), Some("task_complete" | "turn_aborted")) => {
                status = SessionStatus::Idle;
                pending.clear();
            }
            (Some("response_item"), Some("function_call")) => {
                let escalated = p
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|a| serde_json::from_str::<Value>(a).ok())
                    .and_then(|a| a.get("sandbox_permissions").and_then(Value::as_str).map(|s| s == "require_escalated"))
                    .unwrap_or(false);
                if let (true, Some(id)) = (escalated, p.get("call_id").and_then(Value::as_str)) {
                    pending.insert(id.to_owned());
                }
            }
            (Some("response_item"), Some("function_call_output")) => {
                if let Some(id) = p.get("call_id").and_then(Value::as_str) {
                    pending.remove(id);
                }
            }
            _ => {}
        }
    }
    match status {
        SessionStatus::Running if !pending.is_empty() => SessionStatus::Waiting(Some(APPROVAL.to_owned())),
        s => s,
    }
}

/// When the log was last written to, by its entries' own timestamps: the
/// file's modified time can lag by many minutes while Codex keeps it open.
pub fn last_activity(text: &str) -> Option<DateTime<Utc>> {
    text.lines().rev().find_map(|line| {
        let v: Value = serde_json::from_str(line).ok()?;
        Some(DateTime::parse_from_rfc3339(v.get("timestamp")?.as_str()?).ok()?.with_timezone(&Utc))
    })
}

/// `session_index.jsonl` lines: id → latest thread name.
pub fn parse_index(text: &str) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let (Some(id), Some(name)) = (v.get("id").and_then(Value::as_str), v.get("thread_name").and_then(Value::as_str)) {
            if !name.trim().is_empty() {
                names.insert(id.to_owned(), name.trim().to_owned());
            }
        }
    }
    names
}

/// A rollout on disk: its header and when it was last written.
#[derive(Debug, Clone)]
pub struct Rollout {
    pub path: PathBuf,
    pub modified: DateTime<Utc>,
    pub header: Header,
}

/// Picks each CLI process's rollout: the `codex-tui` one it created (header
/// within minutes after the process started), else the newest one written
/// since it started (a resumed conversation). Each rollout is used once.
/// `procs`: (pid, start time), any order. Returns (pid, rollout index).
pub fn match_cli(procs: &[(u32, DateTime<Utc>)], rollouts: &[Rollout]) -> Vec<(u32, usize)> {
    let mut procs = procs.to_vec();
    procs.sort_by_key(|p| p.1);
    let tui: Vec<usize> = (0..rollouts.len()).filter(|&i| rollouts[i].header.originator == "codex-tui").collect();
    let mut used = HashSet::new();
    let mut out = Vec::new();
    // First every process's own new rollout, so a resumed session (second
    // pass) can't take one another process created.
    let mut waiting = Vec::new();
    for (pid, start) in procs {
        let created = tui
            .iter()
            .copied()
            .filter(|i| !used.contains(i))
            .filter(|&i| {
                let d = (rollouts[i].header.started - start).num_seconds();
                (-5..MATCH_WINDOW_SECS).contains(&d)
            })
            .min_by_key(|&i| (rollouts[i].header.started - start).num_seconds().abs());
        match created {
            Some(i) => {
                used.insert(i);
                out.push((pid, i));
            }
            None => waiting.push((pid, start)),
        }
    }
    for (pid, start) in waiting {
        let resumed =
            tui.iter().copied().filter(|i| !used.contains(i)).filter(|&i| rollouts[i].modified >= start).max_by_key(|&i| rollouts[i].modified);
        if let Some(i) = resumed {
            used.insert(i);
            out.push((pid, i));
        }
    }
    out
}

fn sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    Some(home)
}

fn collect(dir: &Path, since: DateTime<Utc>, out: &mut Vec<(DateTime<Utc>, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        if meta.is_dir() {
            collect(&path, since, out);
        } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl")) {
            if let Some(m) = meta.modified().ok().map(DateTime::<Utc>::from).filter(|m| *m >= since) {
                out.push((m, path));
            }
        }
    }
}

fn read_header(path: &Path) -> Option<Header> {
    let mut line = String::new();
    BufReader::new(fs::File::open(path).ok()?).take(HEADER_MAX_BYTES).read_line(&mut line).ok()?;
    parse_header(&line)
}

/// The last `max` bytes of a file, from the first whole line.
fn read_tail(path: &Path, max: u64) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(max).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if start > 0 { text.split_once('\n').map(|(_, r)| r.to_owned()).unwrap_or_default() } else { text })
}

fn to_utc(filetime: u64) -> Option<DateTime<Utc>> {
    // 100 ns ticks since 1601 → Unix seconds.
    let secs = (filetime / 10_000_000) as i64 - 11_644_473_600;
    DateTime::from_timestamp(secs, 0)
}

/// The running Codex sessions, or none when Codex isn't running.
pub fn sessions(own: &HashSet<u32>) -> Vec<Session> {
    let procs = proc::processes();
    let names: HashMap<u32, &str> = procs.iter().map(|p| (p.pid, p.name.as_str())).collect();
    let mut cli = Vec::new();
    let mut desktop_since: Option<DateTime<Utc>> = None;
    for p in procs.iter().filter(|p| p.name.eq_ignore_ascii_case("codex.exe") && !own.contains(&p.pid)) {
        let Some(cmd) = proc::command_line(p.pid) else { continue };
        let Some(start) = proc::process_start_filetime(p.pid).and_then(to_utc) else { continue };
        let parent = names.get(&p.parent).copied().unwrap_or("");
        if is_interactive(&cmd) {
            cli.push((p.pid, start));
        } else if parent.eq_ignore_ascii_case("ChatGPT.exe") && cmd.contains("app-server") {
            desktop_since = Some(desktop_since.map_or(start, |d| d.min(start)));
        }
    }
    if cli.is_empty() && desktop_since.is_none() {
        return Vec::new();
    }
    let Some(root) = sessions_root() else { return Vec::new() };
    // Only rollouts written since the oldest relevant process started, by
    // file time. Desktop gets a week's slack: its latest conversation may be
    // older, and a file kept open can show a stale modified time.
    let oldest = cli.iter().map(|c| c.1).chain(desktop_since.map(|d| d - chrono::Duration::days(7))).min().unwrap_or_else(Utc::now);
    let mut files = Vec::new();
    collect(&root.join("sessions"), oldest - chrono::Duration::minutes(5), &mut files);
    let rollouts: Vec<Rollout> =
        files.into_iter().filter_map(|(modified, path)| Some(Rollout { header: read_header(&path)?, path, modified })).collect();
    let titles = read_tail(&root.join("session_index.jsonl"), INDEX_TAIL_BYTES).map(|t| parse_index(&t)).unwrap_or_default();

    let row = |pid: u32, r: &Rollout, tail: Option<&str>, entrypoint: &str| Session {
        pid,
        name: titles.get(&r.header.id).cloned().unwrap_or_else(|| "Codex".to_owned()),
        status: tail.map_or(SessionStatus::Unknown, status_from_tail),
        cwd: r.header.cwd.clone(),
        entrypoint: Some(entrypoint.to_owned()),
        tool: Tool::Codex,
    };
    let mut out: Vec<Session> = match_cli(&cli, &rollouts)
        .into_iter()
        .map(|(pid, i)| row(pid, &rollouts[i], read_tail(&rollouts[i].path, TAIL_BYTES).as_deref(), "codex-cli"))
        .collect();
    if desktop_since.is_some() {
        // The conversation Desktop wrote to last, by the log's own entries.
        let latest = rollouts
            .iter()
            .filter(|r| r.header.originator == "Codex Desktop")
            .filter_map(|r| {
                let tail = read_tail(&r.path, TAIL_BYTES)?;
                Some((last_activity(&tail).unwrap_or(r.modified), r, tail))
            })
            .max_by_key(|(t, _, _)| *t);
        if let Some((_, r, tail)) = latest {
            // No single PID stands for a Desktop conversation; 0 keeps the
            // row stable and never collides with a real process.
            out.push(row(0, r, Some(&tail), DESKTOP_ENTRYPOINT));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn interactive_command_lines() {
        assert!(is_interactive(r#""C:\npm\codex.exe""#));
        assert!(is_interactive(r"C:\npm\codex.exe resume --last"));
        assert!(is_interactive(r#""C:\Program Files\codex\codex.exe" -m gpt-6"#));
        assert!(!is_interactive(r#""C:\npm\codex.exe" app-server"#));
        assert!(!is_interactive(r"C:\x\codex.exe -c a=1 app-server --listen ws://127.0.0.1:1"));
        assert!(!is_interactive(r#"codex.exe -c "forced_login_method=\"chatgpt\"" exec-server --remote x"#));
        assert!(!is_interactive(r"codex.exe app-server daemon pid-update-loop"));
        // A folder named like a subcommand in the exe path doesn't count.
        assert!(is_interactive(r#""C:\exec\codex.exe""#));
    }

    #[test]
    fn header_and_status() {
        let h = parse_header(r#"{"timestamp":"x","type":"session_meta","payload":{"id":"t1","timestamp":"2026-10-20T07:28:06.123Z","originator":"codex-tui","cwd":"C:\\work\\dbgrab","base_instructions":{"text":"…"}}}"#).unwrap();
        assert_eq!((h.id.as_str(), h.originator.as_str(), h.cwd.as_deref()), ("t1", "codex-tui", Some(r"C:\work\dbgrab")));
        assert_eq!(h.started, Utc.with_ymd_and_hms(2026, 10, 20, 7, 28, 6).unwrap() + chrono::Duration::milliseconds(123));
        assert_eq!(parse_header(r#"{"type":"event_msg","payload":{}}"#), None);

        let ev = |t: &str| format!(r#"{{"type":"event_msg","payload":{{"type":"{t}"}}}}"#);
        let other = r#"{"type":"response_item","payload":{"type":"message","content":"task_complete is just text here"}}"#;
        assert_eq!(status_from_tail(&[ev("task_started"), ev("token_count")].join("\n")), SessionStatus::Running);
        assert_eq!(status_from_tail(&[ev("task_started"), ev("task_complete"), ev("thread_settings_applied")].join("\n")), SessionStatus::Idle);
        assert_eq!(status_from_tail(&[ev("task_complete"), ev("task_started"), other.to_string()].join("\n")), SessionStatus::Running);
        assert_eq!(status_from_tail(&[ev("task_started"), ev("turn_aborted")].join("\n")), SessionStatus::Idle);
        assert_eq!(status_from_tail(&ev("token_count")), SessionStatus::Unknown);
    }

    #[test]
    fn escalated_call_without_output_is_waiting() {
        let ev = |t: &str| format!(r#"{{"type":"event_msg","payload":{{"type":"{t}"}}}}"#);
        let call = |id: &str, escalated: bool| {
            let extra = if escalated { r#",\"sandbox_permissions\":\"require_escalated\",\"justification\":\"j\""# } else { "" };
            format!(r#"{{"type":"response_item","payload":{{"type":"function_call","name":"exec_command","call_id":"{id}","arguments":"{{\"cmd\":\"x\"{extra}}}"}}}}"#)
        };
        let out = |id: &str| format!(r#"{{"type":"response_item","payload":{{"type":"function_call_output","call_id":"{id}","output":"ok"}}}}"#);
        let wait = SessionStatus::Waiting(Some(APPROVAL.into()));
        // Asked, not answered yet.
        assert_eq!(status_from_tail(&[ev("task_started"), call("a", false), out("a"), call("b", true)].join("\n")), wait);
        // Answered: back to RUN, then IDLE when the turn ends.
        assert_eq!(status_from_tail(&[ev("task_started"), call("b", true), out("b")].join("\n")), SessionStatus::Running);
        assert_eq!(status_from_tail(&[ev("task_started"), call("b", true), ev("turn_aborted")].join("\n")), SessionStatus::Idle);
        // A plain command without output is just running.
        assert_eq!(status_from_tail(&[ev("task_started"), call("c", false)].join("\n")), SessionStatus::Running);
        // An unanswered call from an earlier turn doesn't carry over.
        assert_eq!(status_from_tail(&[ev("task_started"), call("d", true), ev("task_complete"), ev("task_started")].join("\n")), SessionStatus::Running);
    }

    #[test]
    fn last_activity_reads_the_newest_entry() {
        let t = "{\"timestamp\":\"2026-10-20T07:42:18.000Z\",\"type\":\"x\"}
{\"timestamp\":\"2026-10-20T07:54:57.500Z\",\"type\":\"y\"}
not json";
        assert_eq!(last_activity(t), Some(Utc.with_ymd_and_hms(2026, 10, 20, 7, 54, 57).unwrap() + chrono::Duration::milliseconds(500)));
        assert_eq!(last_activity("nothing"), None);
    }

    #[test]
    fn index_keeps_latest_name() {
        let t = "{\"id\":\"a\",\"thread_name\":\"Old\"}\n{\"id\":\"b\",\"thread_name\":\"B\"}\n{\"id\":\"a\",\"thread_name\":\"New\"}\nnot json";
        let m = parse_index(t);
        assert_eq!((m["a"].as_str(), m["b"].as_str()), ("New", "B"));
    }

    #[test]
    fn cli_processes_match_their_rollouts() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 20, 7, 0, 0).unwrap();
        let ro = |id: &str, origin: &str, started_min: i64, modified_min: i64| Rollout {
            path: PathBuf::from(id),
            modified: t0 + chrono::Duration::minutes(modified_min),
            header: Header { id: id.into(), started: t0 + chrono::Duration::minutes(started_min), originator: origin.into(), cwd: None },
        };
        let rollouts = vec![
            ro("new-a", "codex-tui", 0, 30),     // created by process A
            ro("desk", "Codex Desktop", 1, 40),  // never a CLI's
            ro("old", "codex-tui", -600, 50),    // resumed by process B
            ro("new-c", "codex-tui", 61, 61),    // created by process C
        ];
        let at = |m: i64| t0 + chrono::Duration::minutes(m);
        let mut got = match_cli(&[(3, at(60)), (1, at(0)), (2, at(20))], &rollouts);
        got.sort();
        assert_eq!(got, vec![(1, 0), (2, 2), (3, 3)]);
        // A process with nothing written since it started has no row.
        assert!(match_cli(&[(9, at(100))], &rollouts).is_empty());
    }
}
