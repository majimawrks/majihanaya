//! Debug builds only: made-up state for README screenshots, so no real
//! account, project or session shows up in them.
//!
//! `MAJIHANAYA_DEMO=1` starts with this state and no workers.
//! `MAJIHANAYA_SHOT=<file>` saves one frame (u32 LE width, u32 LE height,
//! then RGBA rows) once the screen has settled, and exits.
//! `MAJIHANAYA_VIEW=details|settings` (in `ui`) picks the view.

use crate::model::*;
use chrono::{Duration, Utc};

pub fn enabled() -> bool {
    std::env::var_os("MAJIHANAYA_DEMO").is_some()
}

pub fn shot_path() -> Option<std::path::PathBuf> {
    std::env::var_os("MAJIHANAYA_SHOT").map(Into::into)
}

pub fn state() -> AppState {
    let now = Utc::now();
    let mut s = AppState::default();

    let c = &mut s.claude;
    c.presence = Presence::Found;
    c.source = ClaudeSource::Cli;
    c.last_success = Some(now);
    c.account = Some(ClaudeAccount {
        logged_in: true,
        email: Some("you@example.com".into()),
        subscription_type: Some("max".into()),
        auth_method: None,
        org_id: None,
    });
    c.limits.session = Some(Limit { percent: 42, resets_at: Some(now + Duration::minutes(238)) });
    c.limits.week = Some(Limit { percent: 64, resets_at: Some(now + Duration::minutes(6960)) });
    c.limits.model_weeks = vec![("Fable".into(), Limit { percent: 18, resets_at: Some(now + Duration::minutes(6960)) })];
    let stats = |requests, sessions, lines: &[&str]| UsageStats { requests, sessions, lines: lines.iter().map(|l| l.to_string()).collect() };
    c.limits.last_24h = Some(stats(312, 9, &["62% of your usage was at >150k context", "Top skills: /review 12%"]));
    c.limits.last_7d = Some(stats(1874, 41, &["58% of your usage was at >150k context", "Top skills: /review 9%"]));

    let x = &mut s.codex;
    x.presence = Presence::Found;
    x.source = CodexSource::AppServer;
    x.last_success = Some(now);
    x.account = Some(CodexAccount { kind: Some("chatgpt".into()), email: Some("other@example.com".into()), plan_type: Some("plus".into()) });
    x.windows = vec![CodexWindow {
        label: "week".into(),
        bucket: None,
        window_mins: Some(10080),
        percent: 38.0,
        resets_at: Some(now + Duration::minutes(4140)),
    }];

    let session = |pid, name: &str, cwd: &str, status, entrypoint: Option<&str>| Session {
        pid,
        name: name.into(),
        status,
        cwd: Some(cwd.into()),
        entrypoint: entrypoint.map(Into::into),
    };
    s.sessions.list = vec![
        session(1, "Plan review", r"C:\work\garden-api", SessionStatus::Waiting(Some("permission".into())), None),
        session(2, "Refactor the invoice export", r"C:\work\billing", SessionStatus::Working, None),
        session(3, "Notes cleanup", r"C:\work\notes", SessionStatus::Idle, Some("claude-desktop")),
    ];
    s.sessions.last_success = Some(now);

    for st in [&mut s.claude_status, &mut s.codex_status] {
        *st = ServiceStatus { level: StatusLevel::Green, text: String::new(), last_success: Some(now), error: None };
    }
    s.version = VersionState {
        local: Some("2.1.302".into()),
        latest: Some("2.1.302".into()),
        channel: Some("latest".into()),
        last_success: Some(now),
    };
    s
}

/// Writes a captured frame in the raw format described above.
pub fn save(path: &std::path::Path, image: &eframe::egui::ColorImage) {
    let mut out = Vec::with_capacity(8 + image.pixels.len() * 4);
    out.extend_from_slice(&(image.size[0] as u32).to_le_bytes());
    out.extend_from_slice(&(image.size[1] as u32).to_le_bytes());
    for px in &image.pixels {
        out.extend_from_slice(&px.to_srgba_unmultiplied());
    }
    let _ = std::fs::write(path, out);
}
