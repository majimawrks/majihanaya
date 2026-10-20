//! Claude usage: `claude -p /usage` (primary), `claude auth status` (account),
//! and Claude Desktop's `plan-usage-history.json` as a guarded fallback.
//! Never reads Desktop's `config.json` or any token cache.

use crate::config::Config;
use crate::model::*;
use crate::proc;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, TimeZone, Utc};
use regex::Regex;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

const USAGE_ARGS: [&str; 5] = ["-p", "/usage", "--output-format", "text", "--no-session-persistence"];
const USAGE_TIMEOUT: Duration = Duration::from_secs(30);
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
/// A Desktop sample older than this is not trusted.
const DESKTOP_MAX_AGE_SECS: i64 = 10 * 60;

/// Unix seconds of the last `auth status` attempt (0 = never).
static LAST_AUTH_ATTEMPT: AtomicI64 = AtomicI64::new(0);

type DesktopSample = (DateTime<Utc>, Option<String>, u8, u8);

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid regex"))
}

// ---------------------------------------------------------------- parsing

fn month_number(s: &str) -> Option<u32> {
    const M: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let l = s.to_ascii_lowercase();
    M.iter().position(|m| l.starts_with(m)).map(|i| i as u32 + 1)
}

/// `Oct 17, 3:40pm (Asia/Jakarta)` to UTC. Year is `now`'s, +1 if that lands
/// more than 30 days in the past. Unknown or missing zone means local time.
fn parse_reset(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    static R: OnceLock<Regex> = OnceLock::new();
    let c = re(&R, r"(?i)([a-z]{3})[a-z]*\.?\s+(\d{1,2}),?\s+(\d{1,2})(?::(\d{2}))?\s*([ap])m(?:\s*\(([^)]*)\))?")
        .captures(text)?;
    let month = month_number(&c[1])?;
    let day: u32 = c[2].parse().ok()?;
    let mut hour: u32 = c[3].parse().ok()?;
    let minute: u32 = c.get(4).map_or(Some(0), |m| m.as_str().parse().ok())?;
    if !(1..=12).contains(&hour) || minute > 59 {
        return None;
    }
    hour %= 12;
    if c[5].eq_ignore_ascii_case("p") {
        hour += 12;
    }
    let tz: Option<chrono_tz::Tz> = c.get(6).and_then(|z| z.as_str().trim().parse().ok());
    let at_year = |year: i32| -> Option<DateTime<Utc>> {
        let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, 0)?;
        match tz {
            Some(tz) => tz.from_local_datetime(&naive).earliest().map(|d| d.with_timezone(&Utc)),
            None => Local.from_local_datetime(&naive).earliest().map(|d| d.with_timezone(&Utc)),
        }
    };
    // The stamp's year is the year *in its own zone*: at 00:30 UTC on Jan 1,
    // New York is still in December.
    let year = match tz {
        Some(tz) => now.with_timezone(&tz).year(),
        None => now.with_timezone(&Local).year(),
    };
    let this = at_year(year)?;
    if now - this > ChronoDuration::days(30) { at_year(year + 1) } else { Some(this) }
}

fn limit_from(caps: &regex::Captures, now: DateTime<Utc>) -> Option<Limit> {
    let percent = caps[1].parse::<u32>().ok()?.min(100) as u8;
    let resets_at = caps.get(2).and_then(|m| parse_reset(m.as_str(), now));
    Some(Limit { percent, resets_at })
}

fn is_stats_header(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)^\s*Last\s+(24h|7d)\b").is_match(line)
}

fn parse_stats(text: &str, which: &str) -> Option<UsageStats> {
    static R: OnceLock<Regex> = OnceLock::new();
    let hdr = re(&R, r"(?i)^\s*Last\s+(24h|7d)\D*?(\d+)\s*requests?\D*?(\d+)\s*sessions?");
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(c) = hdr.captures(line) else { continue };
        if !c[1].eq_ignore_ascii_case(which) {
            continue;
        }
        let mut body = Vec::new();
        for l in lines.by_ref() {
            if l.trim().is_empty() || is_stats_header(l) {
                break;
            }
            body.push(l.trim().to_string());
        }
        return Some(UsageStats { requests: c[2].parse().ok()?, sessions: c[3].parse().ok()?, lines: body });
    }
    None
}

/// Parses `claude -p /usage` text. `None` unless session or week was found.
pub fn parse_usage(text: &str, now: DateTime<Utc>) -> Option<ClaudeLimits> {
    static S: OnceLock<Regex> = OnceLock::new();
    static W: OnceLock<Regex> = OnceLock::new();
    static M: OnceLock<Regex> = OnceLock::new();
    let session = re(&S, r"(?i)Current session:\s*(\d+)%(?:[^\r\n]*?resets\s*([^\r\n]+))?");
    let week = re(&W, r"(?i)Current week \(all models\):\s*(\d+)%(?:[^\r\n]*?resets\s*([^\r\n]+))?");
    let model = re(&M, r"(?i)Current week \(([^)]+)\):\s*(\d+)%(?:[^\r\n]*?resets\s*([^\r\n]+))?");

    let mut out = ClaudeLimits {
        session: session.captures(text).and_then(|c| limit_from(&c, now)),
        week: week.captures(text).and_then(|c| limit_from(&c, now)),
        ..Default::default()
    };
    for c in model.captures_iter(text) {
        let name = c[1].trim();
        if name.eq_ignore_ascii_case("all models") {
            continue;
        }
        let Ok(p) = c[2].parse::<u32>() else { continue };
        let resets_at = c.get(3).and_then(|m| parse_reset(m.as_str(), now));
        out.model_weeks.push((name.to_string(), Limit { percent: p.min(100) as u8, resets_at }));
    }
    if out.session.is_none() && out.week.is_none() {
        return None;
    }
    out.last_24h = parse_stats(text, "24h");
    out.last_7d = parse_stats(text, "7d");
    Some(out)
}

pub fn parse_auth(json: &str) -> Option<ClaudeAccount> {
    let v: Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(ClaudeAccount {
        logged_in: v.get("loggedIn")?.as_bool()?,
        email: s("email"),
        subscription_type: s("subscriptionType"),
        auth_method: s("authMethod"),
        org_id: s("orgId"),
    })
}

/// Newest sample of `plan-usage-history.json`: (time, org, 5h %, 7d %).
pub fn parse_desktop_history(json: &str) -> Option<(DateTime<Utc>, Option<String>, u8, u8)> {
    let v: Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).ok()?;
    let pct = |x: &Value| -> Option<u8> {
        let f = x.as_f64()?;
        (f.is_finite() && f >= 0.0).then(|| f.round().min(100.0) as u8)
    };
    v.get("samples")?
        .as_array()?
        .iter()
        .filter_map(|s| {
            let t = DateTime::from_timestamp_millis(s.get("t")?.as_i64()?)?;
            let u = s.get("u")?;
            let org = s.get("org").and_then(Value::as_str).map(str::to_string);
            Some((t, org, pct(u.get("fh")?)?, pct(u.get("sd")?)?))
        })
        .max_by_key(|s| s.0)
}

// ------------------------------------------------------- fallback decision

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// Do not use Desktop data (and clear any org-mismatch flag).
    No,
    /// Use these Desktop numbers (5h %, 7d %).
    Use(u8, u8),
    /// Sample belongs to another org: show a warning, not its numbers.
    OrgMismatch,
}

/// Pure fallback policy. `cli_fresh`: the CLI answered within the stale window.
pub fn decide_fallback(
    enabled: bool,
    cli_fresh: bool,
    sample: Option<&DesktopSample>,
    cli_org: Option<&str>,
    now: DateTime<Utc>,
) -> Fallback {
    if !enabled || cli_fresh {
        return Fallback::No;
    }
    let Some((t, org, fh, sd)) = sample else { return Fallback::No };
    let age = (now - *t).num_seconds();
    if !(-60..DESKTOP_MAX_AGE_SECS).contains(&age) {
        return Fallback::No;
    }
    if let Some(cli_org) = cli_org {
        if org.as_deref() != Some(cli_org) {
            return Fallback::OrgMismatch;
        }
    }
    Fallback::Use(*fh, *sd)
}

// ------------------------------------------------------------ CLI lookup

fn semver_key(name: &str) -> Option<(u64, u64, u64)> {
    let mut it = name.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

fn find_claude_exe(dir: &Path, depth: u32) -> Option<PathBuf> {
    let direct = dir.join("claude.exe");
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .find_map(|e| find_claude_exe(&e.path(), depth - 1))
}

/// Subfolders of `dir` whose name passes `keep` (unreadable: none).
fn subdirs(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| keep(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Every folder Claude Desktop may keep its data in, depending on the
/// version and how it was installed: `%APPDATA%\Claude`, newer builds'
/// `%LOCALAPPDATA%\Claude-Data\<package>`, and the Store package's
/// virtualized `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude`.
fn desktop_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = dirs::config_dir().map(|d| d.join("Claude")).into_iter().collect();
    if let Some(local) = dirs::data_local_dir() {
        roots.extend(subdirs(&local.join("Claude-Data"), |_| true));
        roots.extend(
            subdirs(&local.join("Packages"), |n| n.to_ascii_lowercase().starts_with("claude_"))
                .into_iter()
                .map(|p| p.join("LocalCache").join("Roaming").join("Claude")),
        );
    }
    roots.retain(|r| r.is_dir());
    roots
}

/// The newest Desktop-bundled CLI across all Desktop data folders.
fn desktop_cli() -> Option<PathBuf> {
    let mut versions: Vec<_> = desktop_roots()
        .iter()
        .flat_map(|root| subdirs(&root.join("claude-code"), |_| true))
        .filter_map(|p| Some((semver_key(&p.file_name()?.to_string_lossy())?, p)))
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().find_map(|(_, p)| find_claude_exe(&p, 3))
}

/// `claude` on PATH, else the newest Desktop-bundled CLI.
pub fn resolve_cli() -> Option<PathBuf> {
    proc::find_on_path("claude").or_else(desktop_cli)
}

// --------------------------------------------------------------- refresh

fn first_line(s: &str) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no output");
    l.chars().take(120).collect()
}

fn probe_usage(state: &Shared, cli: &Path, now: DateTime<Utc>) -> Result<ClaudeLimits, String> {
    let out =
        proc::run_capture(state, cli, &USAGE_ARGS, USAGE_TIMEOUT).map_err(|e| format!("usage probe failed: {e}"))?;
    let text = if out.stdout.trim().is_empty() { &out.stderr } else { &out.stdout };
    parse_usage(text, now).ok_or_else(|| format!("no usage data: {}", first_line(text)))
}

fn probe_auth(state: &Shared, cli: &Path) -> Option<ClaudeAccount> {
    let out = proc::run_capture(state, cli, &["auth", "status"], AUTH_TIMEOUT).ok()?;
    parse_auth(&out.stdout)
}

/// The newest sample across all Desktop data folders.
fn desktop_sample() -> Option<DesktopSample> {
    desktop_roots()
        .iter()
        // Desktop keeps a bounded history; a bigger file is not one we understand.
        .filter_map(|root| proc::read_text_capped(&root.join("plan-usage-history.json"), 8 * 1024 * 1024).ok())
        .filter_map(|text| parse_desktop_history(&text))
        .max_by_key(|s| s.0)
}

/// One usage probe + (if due) auth status + fallback logic.
pub fn refresh(state: &Shared, config: &Config) {
    let now = Utc::now();
    let usage_interval = Duration::from_secs(config.usage_interval_seconds);
    let cli = resolve_cli();

    // Auth first, so the org is known for the fallback check.
    if let Some(cli) = &cli {
        let due = now.timestamp() - LAST_AUTH_ATTEMPT.load(Ordering::Relaxed) >= config.account_interval_seconds as i64;
        if due {
            LAST_AUTH_ATTEMPT.store(now.timestamp(), Ordering::Relaxed);
            if let Some(acc) = probe_auth(state, cli) {
                let mut s = lock(state);
                let c = &mut s.claude;
                let old_org = c.account.as_ref().and_then(|a| a.org_id.clone());
                // Another account's numbers must not pass as this one's: drop
                // their freshness so the next failure can't keep showing them.
                if old_org.is_some() && acc.org_id != old_org {
                    c.last_success = None;
                    c.source = ClaudeSource::None;
                }
                c.account = Some(acc);
            }
        }
    }

    let result = match &cli {
        Some(cli) => probe_usage(state, cli, now),
        None => Err("claude CLI not found".to_string()),
    };

    let now = Utc::now();
    match result {
        Ok(limits) => {
            let mut s = lock(state);
            let c = &mut s.claude;
            c.presence = Presence::Found;
            c.limits = limits;
            c.source = ClaudeSource::Cli;
            c.org_mismatch = false;
            c.last_attempt = Some(now);
            c.last_success = Some(now);
            c.error = None;
        }
        Err(err) => {
            let (cli_fresh, cli_org) = {
                let s = lock(state);
                let c = &s.claude;
                (
                    c.source == ClaudeSource::Cli && !is_stale(c.last_success, usage_interval, now),
                    c.account.as_ref().and_then(|a| a.org_id.clone()),
                )
            };
            let sample = if config.use_desktop_fallback && !cli_fresh { desktop_sample() } else { None };
            let decision =
                decide_fallback(config.use_desktop_fallback, cli_fresh, sample.as_ref(), cli_org.as_deref(), now);
            let mut s = lock(state);
            let c = &mut s.claude;
            // Claude Desktop's history alone is a source: show the section
            // (with its numbers, or why there are none) instead of hiding it.
            c.presence = if cli.is_some() || sample.is_some() { Presence::Found } else { Presence::Missing };
            c.last_attempt = Some(now);
            match decision {
                Fallback::Use(fh, sd) => {
                    c.limits = ClaudeLimits {
                        session: Some(Limit { percent: fh, resets_at: None }),
                        week: Some(Limit { percent: sd, resets_at: None }),
                        ..Default::default()
                    };
                    c.source = ClaudeSource::Desktop;
                    c.org_mismatch = false;
                    // As old as the sample, so staleness follows it.
                    c.last_success = sample.as_ref().map(|s| s.0).or(Some(now));
                    c.error = None;
                }
                Fallback::OrgMismatch => {
                    // Which account the retained numbers belong to is now in
                    // doubt: drop them, so the tray, the monitors, the reset
                    // clocks and the stats all stop showing them alike.
                    c.limits = ClaudeLimits::default();
                    c.source = ClaudeSource::None;
                    c.org_mismatch = true;
                    c.error = Some(err);
                }
                Fallback::No => {
                    c.org_mismatch = false;
                    c.error = Some(err);
                }
            }
        }
    }
}

/// Worker thread: refresh, notify, sleep `usage_interval_seconds` from the end
/// of each try (so two probes never overlap). Never panics.
pub fn spawn(state: Shared, notify: Notify) {
    let _ = thread::Builder::new().name("claude-usage".into()).spawn(move || {
        loop {
            // Read every cycle: Settings and config-file edits apply live.
            let config = crate::config::current();
            let _ = catch_unwind(AssertUnwindSafe(|| refresh(&state, &config)));
            let _ = catch_unwind(AssertUnwindSafe(|| notify()));
            crate::model::REFRESH.sleep(Duration::from_secs(config.usage_interval_seconds.max(5)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRO: &str = include_str!("../../tests/fixtures/usage_pro.txt");
    const FABLE: &str = include_str!("../../tests/fixtures/usage_fable.txt");
    const ONE: &str = include_str!("../../tests/fixtures/usage_one_session_no_usage.txt");
    const OUT: &str = include_str!("../../tests/fixtures/usage_logged_out.txt");
    const AUTH: &str = include_str!("../../tests/fixtures/auth_status.json");
    const AUTH_OUT: &str = include_str!("../../tests/fixtures/auth_status_logged_out.json");
    const DESKTOP: &str = include_str!("../../tests/fixtures/desktop_history.json");

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }
    fn now() -> DateTime<Utc> {
        utc(2026, 10, 18, 5, 0)
    }

    #[test]
    fn usage_pro() {
        let l = parse_usage(PRO, now()).unwrap();
        let s = l.session.unwrap();
        assert_eq!(s.percent, 16);
        assert_eq!(s.resets_at, Some(utc(2026, 10, 18, 13, 10))); // 8:10pm +07
        let w = l.week.unwrap();
        assert_eq!(w.percent, 9);
        assert_eq!(w.resets_at, Some(utc(2026, 10, 22, 12, 0))); // 7pm +07
        assert!(l.model_weeks.is_empty());
        let d = l.last_24h.unwrap();
        assert_eq!((d.requests, d.sessions, d.lines.len()), (69, 2, 4));
        assert_eq!(d.lines[0], "70% of your usage was at >150k context");
        let w7 = l.last_7d.unwrap();
        assert_eq!((w7.requests, w7.sessions), (218, 3));
        assert_eq!(w7.lines.len(), 6);
    }

    #[test]
    fn usage_fable() {
        let l = parse_usage(FABLE, now()).unwrap();
        assert_eq!(l.week.as_ref().unwrap().percent, 26);
        assert_eq!(l.model_weeks.len(), 1);
        assert_eq!(l.model_weeks[0].0, "Fable");
        assert_eq!(l.model_weeks[0].1.percent, 19);
        assert_eq!(l.model_weeks[0].1.resets_at, Some(utc(2026, 10, 19, 2, 59))); // 5:59am +03
        assert_eq!(l.session.unwrap().resets_at, Some(utc(2026, 10, 17, 2, 19)));
        assert_eq!(l.last_24h.unwrap().requests, 1043);
    }

    #[test]
    fn usage_one_session_no_usage() {
        let l = parse_usage(ONE, now()).unwrap();
        assert_eq!(l.session, Some(Limit { percent: 0, resets_at: None }));
        let w = l.week.unwrap();
        assert_eq!((w.percent, w.resets_at), (3, Some(utc(2026, 10, 22, 12, 0)))); // 7PM
        let d = l.last_24h.unwrap();
        assert_eq!((d.requests, d.sessions), (1, 1));
        assert_eq!(d.lines, vec!["Top skills: /init 100%"]);
        assert_eq!(l.last_7d.unwrap().requests, 12);
    }

    #[test]
    fn usage_logged_out() {
        assert_eq!(parse_usage(OUT, now()), None);
        assert_eq!(parse_usage("", now()), None);
    }

    #[test]
    fn reset_year_rollover_and_zones() {
        let dec30 = utc(2026, 12, 30, 12, 0);
        assert_eq!(parse_reset("Jan 2, 3:40pm (UTC)", dec30), Some(utc(2027, 1, 2, 15, 40)));
        assert_eq!(parse_reset("Dec 31, 1am (UTC)", dec30), Some(utc(2026, 12, 31, 1, 0)));
        // 00:30 UTC on Jan 1 is still Dec 31 in New York: the reset is 90 minutes away.
        let jan1 = utc(2027, 1, 1, 0, 30);
        assert_eq!(parse_reset("Dec 31, 9pm (America/New_York)", jan1), Some(utc(2027, 1, 1, 2, 0)));
        // slightly in the past stays in the current year
        assert_eq!(parse_reset("Dec 29, 12pm (UTC)", dec30), Some(utc(2026, 12, 29, 12, 0)));
        assert_eq!(parse_reset("Oct 17, 12am (UTC)", now()), Some(utc(2026, 10, 17, 0, 0)));
        assert_eq!(parse_reset("garbage", now()), None);
    }

    #[test]
    fn auth_fixtures() {
        let a = parse_auth(AUTH).unwrap();
        assert!(a.logged_in);
        assert_eq!(a.email.as_deref(), Some("user@example.com"));
        assert_eq!(a.subscription_type.as_deref(), Some("pro"));
        assert_eq!(a.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(a.org_id.as_deref(), Some("00000000-0000-0000-0000-000000000001"));
        let o = parse_auth(AUTH_OUT).unwrap();
        assert!(!o.logged_in && o.email.is_none() && o.org_id.is_none());
        assert_eq!(parse_auth("nope"), None);
    }

    #[test]
    fn desktop_history_newest() {
        let (t, org, fh, sd) = parse_desktop_history(DESKTOP).unwrap();
        assert_eq!(t.timestamp_millis(), 1792314195270);
        assert_eq!(org.as_deref(), Some("00000000-0000-0000-0000-000000000001"));
        assert_eq!((fh, sd), (15, 9));
        assert_eq!(parse_desktop_history("{}"), None);
        assert_eq!(parse_desktop_history(r#"{"samples":[{"t":1}]}"#), None);
    }

    #[test]
    fn fallback_decision() {
        let n = now();
        let org = "org-a";
        let fresh = (n - ChronoDuration::minutes(2), Some(org.to_string()), 15, 9);
        let old = (n - ChronoDuration::minutes(11), Some(org.to_string()), 15, 9);
        let other = (n - ChronoDuration::minutes(2), Some("org-b".to_string()), 15, 9);
        let none_org = (n - ChronoDuration::minutes(2), None, 15, 9);
        // CLI fresh wins
        assert_eq!(decide_fallback(true, true, Some(&fresh), Some(org), n), Fallback::No);
        // disabled
        assert_eq!(decide_fallback(false, false, Some(&fresh), Some(org), n), Fallback::No);
        // usable
        assert_eq!(decide_fallback(true, false, Some(&fresh), Some(org), n), Fallback::Use(15, 9));
        assert_eq!(decide_fallback(true, false, Some(&fresh), None, n), Fallback::Use(15, 9));
        // too old / missing
        assert_eq!(decide_fallback(true, false, Some(&old), Some(org), n), Fallback::No);
        assert_eq!(decide_fallback(true, false, None, Some(org), n), Fallback::No);
        // other org (or sample without org when CLI org is known)
        assert_eq!(decide_fallback(true, false, Some(&other), Some(org), n), Fallback::OrgMismatch);
        assert_eq!(decide_fallback(true, false, Some(&none_org), Some(org), n), Fallback::OrgMismatch);
    }

    #[test]
    fn subdirs_filters_by_name() {
        let d = std::env::temp_dir().join(format!("majihanaya-subdirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for name in ["Claude_pzs8sxrjxfjjc", "Microsoft.Other", "claude_lower"] {
            std::fs::create_dir_all(d.join(name)).unwrap();
        }
        std::fs::write(d.join("Claude_file"), "").unwrap();
        let mut found: Vec<String> = subdirs(&d, |n| n.to_ascii_lowercase().starts_with("claude_"))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["Claude_pzs8sxrjxfjjc", "claude_lower"]);
        assert!(subdirs(&d.join("missing"), |_| true).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn semver_ordering() {
        assert!(semver_key("1.10.0") > semver_key("1.9.9"));
        assert_eq!(semver_key("not-a-version"), None);
    }
}
