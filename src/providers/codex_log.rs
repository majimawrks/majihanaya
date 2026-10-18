//! A small local record of which Codex usage windows the service reports,
//! written only when that set changes (e.g. a 5-hour window appearing at
//! some hours and not others). `%APPDATA%\majihanaya\codex-windows.log`.
//!
//! Bounded: each file is at most `MAX_BYTES`; when full it rotates to
//! `.1`, `.2`, … keeping `KEEP` old files, so the total never exceeds
//! `(KEEP + 1) × MAX_BYTES`.

use crate::logfile::LogFile;
use crate::model::CodexWindow;
use chrono::{DateTime, Local, Utc};
use std::path::Path;
use std::sync::Mutex;

const MAX_BYTES: u64 = 64 * 1024;
const KEEP: usize = 2;
const NAME: &str = "codex-windows.log";
static LOG: LogFile = LogFile::new(NAME, MAX_BYTES, KEEP);

/// The window set as written: "week (10080 min)", sorted, buckets included.
fn signature(windows: &[CodexWindow]) -> String {
    let mut parts: Vec<String> = windows
        .iter()
        .map(|w| match w.window_mins {
            Some(m) => format!("{} ({m} min)", w.label),
            None => w.label.clone(),
        })
        .collect();
    parts.sort();
    if parts.is_empty() { "none".into() } else { parts.join(" | ") }
}

fn line(windows: &[CodexWindow], now: DateTime<Utc>) -> String {
    let detail: Vec<String> = windows.iter().map(|w| format!("{} {}%", w.label, w.percent.round())).collect();
    format!(
        "{}  windows: {}  ·  {}\n",
        now.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S %:z"),
        signature(windows),
        detail.join(", ")
    )
}

/// Appends a line to `path` when `windows` differs from the last set seen
/// (the first set seen in a run is always written).
fn record_at(log: &LogFile, path: &Path, last: &mut Option<String>, windows: &[CodexWindow], now: DateTime<Utc>) {
    let sig = signature(windows);
    if last.as_deref() == Some(sig.as_str()) {
        return;
    }
    *last = Some(sig);
    log.append_at(path, &line(windows, now));
}

static LAST: Mutex<Option<String>> = Mutex::new(None);

pub fn record(windows: &[CodexWindow]) {
    let Some(path) = crate::config::path().and_then(|p| p.parent().map(|d| d.join(NAME))) else { return };
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    record_at(&LOG, &path, &mut last, windows, Utc::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn w(label: &str, mins: u32, pct: f32) -> CodexWindow {
        CodexWindow { label: label.into(), bucket: None, window_mins: Some(mins), percent: pct, resets_at: None }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("majihanaya-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_only_on_change() {
        let d = temp_dir("change");
        let p = d.join(NAME);
        let mut last = None;
        let now = Utc::now();
        record_at(&LOG, &p, &mut last, &[w("week", 10080, 2.0)], now);
        record_at(&LOG, &p, &mut last, &[w("week", 10080, 3.0)], now); // same set, new %
        record_at(&LOG, &p, &mut last, &[w("5-hour", 300, 10.0), w("week", 10080, 3.0)], now);
        record_at(&LOG, &p, &mut last, &[w("week", 10080, 3.0)], now);
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.contains("5-hour (300 min) | week (10080 min)"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn size_and_file_count_are_capped() {
        let d = temp_dir("cap");
        let p = d.join(NAME);
        let mut last = None;
        let now = Utc::now();
        // Alternate sets so every call writes; far more than all files can hold.
        for i in 0..20_000 {
            let set = if i % 2 == 0 { vec![w("week", 10080, 1.0)] } else { vec![w("5-hour", 300, 1.0), w("week", 10080, 1.0)] };
            record_at(&LOG, &p, &mut last, &set, now);
        }
        let files: Vec<_> = fs::read_dir(&d).unwrap().flatten().collect();
        assert_eq!(files.len(), KEEP + 1);
        for f in &files {
            assert!(f.metadata().unwrap().len() <= MAX_BYTES);
        }
        let _ = fs::remove_dir_all(d);
    }
}
