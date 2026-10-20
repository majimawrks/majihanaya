//! `AppState`: everything the UI shows. Workers write it, the UI reads it.
//!
//! Rules every worker follows:
//! - Lock briefly: compute outside the lock, then swap fields in.
//! - A failed refresh keeps the previous values and only updates
//!   `last_attempt` / `error`; staleness is derived from `last_success`.
//! - After writing, call the `Notify` so the UI repaints.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

pub type Shared = Arc<Mutex<AppState>>;

/// Locks the state, recovering from a poisoned mutex: a worker that panicked
/// mid-write must not take the UI and the other workers down with it.
pub fn lock(state: &Shared) -> std::sync::MutexGuard<'_, AppState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wakes the UI after a worker changed `AppState`.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub claude: ClaudeState,
    pub codex: CodexState,
    pub sessions: SessionsState,
    pub claude_status: ServiceStatus,
    /// The Codex-relevant part of status.openai.com.
    pub codex_status: ServiceStatus,
    pub version: VersionState,
    /// PIDs of our own probe children, so they never count as sessions.
    #[serde(skip)]
    pub own_pids: HashSet<u32>,
}

/// Whether a CLI could be found and used at all.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Presence {
    /// Not checked yet.
    #[default]
    Unknown,
    /// Binary not found anywhere we look.
    Missing,
    /// Found; may still be logged out (see the account).
    Found,
}

/// One usage window: "5-hour", "week", "Fable week", …
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limit {
    pub percent: u8,
    /// `None` when the window has not started (0% with no reset printed) or the
    /// source has no reset times (Desktop fallback).
    pub resets_at: Option<DateTime<Utc>>,
}

/// A "Last 24h · N requests · N sessions" block from `/usage`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub requests: u32,
    pub sessions: u32,
    /// The indented lines below the header, trimmed, kept verbatim.
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLimits {
    pub session: Option<Limit>,
    pub week: Option<Limit>,
    /// Per-model weeks in printed order, e.g. `("Fable", Limit)`.
    pub model_weeks: Vec<(String, Limit)>,
    pub last_24h: Option<UsageStats>,
    pub last_7d: Option<UsageStats>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeSource {
    #[default]
    None,
    Cli,
    /// `%APPDATA%\Claude\plan-usage-history.json`; no resets, stats or model weeks.
    Desktop,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccount {
    pub logged_in: bool,
    pub email: Option<String>,
    /// pro / max / team / enterprise / free, as printed.
    pub subscription_type: Option<String>,
    pub auth_method: Option<String>,
    pub org_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeState {
    pub presence: Presence,
    pub account: Option<ClaudeAccount>,
    pub limits: ClaudeLimits,
    /// Where `limits` came from on the last successful refresh.
    pub source: ClaudeSource,
    /// Desktop history belongs to a different org than the CLI account:
    /// show a warning instead of numbers.
    pub org_mismatch: bool,
    /// Claude Desktop is here but no logged-in Claude Code is (none on
    /// PATH, and Desktop's bundled one has no login of its own): only
    /// Desktop's occasional history is available, and the user is told
    /// that full usage needs Claude Code.
    pub desktop_only: bool,
    pub last_success: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexWindow {
    /// From `windowDurationMins`: 300 → "5-hour", 10080 → "week", else "N h";
    /// prefixed with the bucket's name for buckets other than the main one.
    pub label: String,
    /// `None` for the main bucket (`rateLimits`); else the extra bucket's
    /// `limitId` from `rateLimitsByLimitId`.
    pub bucket: Option<String>,
    pub window_mins: Option<u32>,
    pub percent: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccount {
    /// e.g. "chatgpt", "apiKey".
    pub kind: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodexSource {
    #[default]
    None,
    AppServer,
    /// Newest `~/.codex/sessions/**/rollout-*.jsonl` `token_count` event.
    Rollout,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexState {
    pub presence: Presence,
    pub account: Option<CodexAccount>,
    pub windows: Vec<CodexWindow>,
    pub source: CodexSource,
    /// For `Rollout`: when the event was written (the data is that old).
    pub source_time: Option<DateTime<Utc>>,
    /// Server version from the `initialize` reply's user agent, if parsed.
    pub server_version: Option<String>,
    /// `account/read` has answered at least once, so `account: None` means
    /// logged out rather than not asked yet.
    #[serde(skip)]
    pub account_read: bool,
    /// When the signed-in identity last changed (or was found logged out):
    /// rollout events older than this may be another account's.
    #[serde(skip)]
    pub identity_since: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "for")]
pub enum SessionStatus {
    Working,
    Waiting(Option<String>),
    Idle,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub pid: u32,
    pub name: String,
    pub status: SessionStatus,
    pub cwd: Option<String>,
    /// e.g. "claude-desktop"; `None` for the plain CLI.
    pub entrypoint: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsState {
    pub list: Vec<Session>,
    pub last_success: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusLevel {
    #[default]
    Unknown,
    Green,
    Yellow,
    Red,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub level: StatusLevel,
    /// Active incident names, else "maintenance", else "some issues"; empty when green.
    pub text: String,
    pub last_success: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

impl ServiceStatus {
    /// The level, or `Unknown` once the last good fetch is stale: while the
    /// page can't be reached, an old "operational" must not stand.
    pub fn level_at(&self, interval: Duration, now: DateTime<Utc>) -> StatusLevel {
        if is_stale(self.last_success, interval, now) { StatusLevel::Unknown } else { self.level }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionState {
    pub local: Option<String>,
    pub latest: Option<String>,
    pub channel: Option<String>,
    pub last_success: Option<DateTime<Utc>>,
}

/// "Refresh now": workers sleep through this instead of `thread::sleep`, so
/// a tray click wakes them all at once. Hooks wake workers that wait on
/// something else (the Codex worker's reply channel).
pub struct Refresh {
    epoch: Mutex<u64>,
    cv: Condvar,
    /// (id, hook); ids come from `next_hook`.
    hooks: Mutex<Vec<(u64, Hook)>>,
    next_hook: Mutex<u64>,
}

type Hook = Box<dyn Fn() -> bool + Send>;

/// Unregisters its hook when dropped, so a hook (and whatever it holds)
/// lives exactly as long as the worker state it wakes.
#[must_use = "dropping the guard unregisters the hook at once"]
pub struct HookGuard {
    owner: &'static Refresh,
    id: u64,
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        self.owner.hooks.lock().unwrap_or_else(|e| e.into_inner()).retain(|(id, _)| *id != self.id);
    }
}

pub static REFRESH: LazyLock<Refresh> = LazyLock::new(Refresh::new);

impl Refresh {
    fn new() -> Self {
        Refresh { epoch: Mutex::new(0), cv: Condvar::new(), hooks: Mutex::new(Vec::new()), next_hook: Mutex::new(0) }
    }

    pub fn epoch(&self) -> u64 {
        *self.epoch.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wakes every sleeping worker.
    pub fn trigger(&self) {
        *self.epoch.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_all();
        // A hook returning false is dead (its worker is gone) and is dropped.
        self.hooks.lock().unwrap_or_else(|e| e.into_inner()).retain(|(_, h)| h());
    }

    /// Registers `hook` until the returned guard is dropped.
    pub fn add_hook(&'static self, hook: Hook) -> HookGuard {
        let id = {
            let mut n = self.next_hook.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            *n
        };
        self.hooks.lock().unwrap_or_else(|e| e.into_inner()).push((id, hook));
        HookGuard { owner: self, id }
    }

    #[cfg(test)]
    fn hook_count(&self) -> usize {
        self.hooks.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Sleeps up to `dur`; returns early when `trigger` is called.
    pub fn sleep(&self, dur: Duration) {
        let guard = self.epoch.lock().unwrap_or_else(|e| e.into_inner());
        let start = *guard;
        let _ = self.cv.wait_timeout_while(guard, dur, |e| *e == start);
    }
}

/// Stale when the last success is older than `3 × interval + 30 s` (or never).
pub fn is_stale(last_success: Option<DateTime<Utc>>, interval: Duration, now: DateTime<Utc>) -> bool {
    let Some(t) = last_success else { return true };
    let limit = chrono::Duration::from_std(interval * 3 + Duration::from_secs(30)).unwrap_or(chrono::Duration::MAX);
    now - t > limit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_window() {
        let now = Utc::now();
        let i = Duration::from_secs(60);
        assert!(is_stale(None, i, now));
        assert!(!is_stale(Some(now - chrono::Duration::seconds(200)), i, now));
        assert!(is_stale(Some(now - chrono::Duration::seconds(211)), i, now));
    }

    #[test]
    fn hook_guard_unregisters() {
        static R: LazyLock<Refresh> = LazyLock::new(Refresh::new);
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let f = fired.clone();
        let guard = R.add_hook(Box::new(move || {
            f.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }));
        R.trigger();
        assert_eq!((R.hook_count(), fired.load(std::sync::atomic::Ordering::SeqCst)), (1, 1));
        drop(guard);
        assert_eq!(R.hook_count(), 0);
        R.trigger();
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn service_status_goes_unknown_when_stale() {
        let now = Utc::now();
        let i = Duration::from_secs(120);
        let mut s = ServiceStatus { level: StatusLevel::Green, last_success: Some(now), ..Default::default() };
        assert_eq!(s.level_at(i, now), StatusLevel::Green);
        // Fetches failing for a while after one good one: no longer "OK".
        s.last_success = Some(now - chrono::Duration::minutes(10));
        s.error = Some("offline".into());
        assert_eq!(s.level_at(i, now), StatusLevel::Unknown);
    }
}
