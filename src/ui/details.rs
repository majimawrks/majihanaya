//! Details view (632 wide, two columns): see the design's Expanded board.

use super::feeds::{Feed, Wall, left, phosphor_for};
use super::monitor::{self, Fx, text};
use super::textures::Textures;
use super::theme::{self, Phosphor, chrome, mono, vt};
use crate::config::Config;
use crate::model::{AppState, ServiceStatus, Session, SessionStatus, StatusLevel, UsageStats};
use chrono::{DateTime, FixedOffset, Utc};
use eframe::egui::{self, Align2, Color32, FontId, Painter, Rect, pos2, vec2};

const GAP: f32 = 8.0;
/// Bezel padding on both sides of a screen.
const BEZEL: f32 = monitor::BEZEL_PAD * 2.0;
const SESSIONS_H: f32 = 128.0;
const CLOCKS_H: f32 = 104.0;
const STATUS_MIN: f32 = 62.0;
const TRAFFIC_MIN: f32 = 172.0;
const RIGHT_GAP: f32 = 6.0;

/// Draws the details body (everything between title bar and footer).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &mut egui::Ui,
    tex: &Textures,
    wall: &Wall,
    state: &AppState,
    config: &Config,
    now: DateTime<Utc>,
    tz: FixedOffset,
    blink_on: bool,
    fx: &dyn Fn(&Feed) -> Fx,
) {
    let col_w = (ui.available_width() - GAP) / 2.0;
    let left_used = sections_height(wall);
    let left_gap = if left_used > 0.0 { GAP } else { 0.0 };
    let sessions_h = if config.show_sessions { SESSIONS_H + BEZEL + RIGHT_GAP } else { 0.0 };
    let right_fixed = sessions_h + CLOCKS_H + BEZEL + RIGHT_GAP;
    let left_base = left_used + if config.show_status { left_gap + STATUS_MIN + BEZEL } else { 0.0 };
    let right_base = right_fixed + TRAFFIC_MIN + BEZEL;
    let total = left_base.max(right_base);

    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), total), egui::Sense::hover());
    let left_rect = Rect::from_min_size(area.min, vec2(col_w, total));
    let right_rect = Rect::from_min_size(pos2(area.min.x + col_w + GAP, area.min.y), vec2(col_w, total));

    ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        for sec in &wall.sections {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let email = if config.show_account { sec.email.as_deref() } else { None };
                monitor::plate(ui, col_w, sec.name, &sec.plan, sec.plan_color, email);
                if let Some(n) = &sec.notice {
                    monitor::notice(ui, col_w, n);
                }
                for f in &sec.feeds {
                    monitor::feed_monitor(ui, tex, col_w, f, fx(f));
                }
            });
        }
        if config.show_status {
            let h = total - left_used - left_gap - BEZEL;
            monitor::monitor(ui, tex, col_w, h, theme::OK.bg, None, Fx::default(), |p, r| {
                status_screen(p, r, state, tz);
            });
        }
    });

    ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
        ui.spacing_mut().item_spacing = vec2(GAP, RIGHT_GAP);
        let bg = theme::OK.bg;
        if config.show_sessions {
            let waiting = state.sessions.list.iter().any(|s| matches!(s.status, SessionStatus::Waiting(_)));
            let stroke = (waiting && blink_on).then_some(chrome::AMBER);
            let mut desktop_rows = Vec::new();
            monitor::monitor(ui, tex, col_w, SESSIONS_H, bg, stroke, Fx::default(), |p, r| {
                desktop_rows = sessions_screen(p, r, state, tz, blink_on);
            });
            for (i, row) in desktop_rows.into_iter().enumerate() {
                ui.interact(row, ui.scope_id().with(("desktop-session", i)), egui::Sense::hover())
                    .on_hover_text(DESKTOP_NOTE);
            }
        }
        monitor::monitor(ui, tex, col_w, CLOCKS_H, bg, None, Fx::default(), |p, r| {
            clocks_screen(p, r, &clock_rows(state, config, now, tz), tz);
        });
        let h = total - right_fixed - BEZEL;
        monitor::monitor(ui, tex, col_w, h, bg, None, Fx::default(), |p, r| {
            traffic_screen(p, r, state, config);
        });
    });
}

/// Height of the usage sections plus the gaps between them (no trailing gap).
fn sections_height(wall: &Wall) -> f32 {
    let secs: Vec<f32> = wall
        .sections
        .iter()
        .map(|s| {
            let mut items = vec![18.0];
            if s.notice.is_some() {
                items.push(22.0);
            }
            items.extend(s.feeds.iter().map(|f| f.screen_height() + BEZEL));
            items.iter().sum::<f32>() + 6.0 * (items.len() - 1) as f32
        })
        .collect();
    secs.iter().sum::<f32>() + GAP * secs.len().saturating_sub(1) as f32
}

/// The 16 px screen header: label left, tag right.
fn header(p: &Painter, r: Rect, left: &str, right: &str) {
    text(p, r.left_top(), Align2::LEFT_TOP, left, vt(17.0), theme::OK.dim, 0.0);
    text(p, r.right_top(), Align2::RIGHT_TOP, right, vt(17.0), theme::OK.dim, 0.0);
}

fn clock_of(t: Option<DateTime<Utc>>, tz: FixedOffset) -> String {
    t.map_or("--:--:--".into(), |t| t.with_timezone(&tz).format("%H:%M:%S").to_string())
}

/// `s` shortened with an ellipsis so it fits `max_w`.
fn fit(p: &Painter, s: &str, font: &FontId, max_w: f32) -> String {
    let width = |t: &str| p.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(s) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    for n in (0..chars.len()).rev() {
        let t = chars[..n].iter().collect::<String>() + "…";
        if width(&t) <= max_w {
            return t;
        }
    }
    "…".into()
}

fn status_screen(p: &Painter, r: Rect, state: &AppState, tz: FixedOffset) {
    header(p, r, "CAM 07 · STATUS", &clock_of(state.claude_status.last_success, tz));
    let rows = [("CLAUDE", Some(&state.claude_status)), ("CODEX", Some(&state.codex_status))];
    for (i, (name, status)) in rows.into_iter().enumerate() {
        let y = r.top() + 16.0 + 3.0 + i as f32 * 19.0 + 8.0;
        let (color, label) = match status {
            Some(s) => status_look(s),
            None => (theme::NO_DATA.fg, "--".to_string()),
        };
        p.circle_filled(pos2(r.left() + 3.5, y), 3.5, color);
        text(p, pos2(r.left() + 15.0, y), Align2::LEFT_CENTER, name, vt(18.0), color, 0.0);
        let x = r.left() + 15.0 + 56.0 + 8.0;
        let label = fit(p, &label, &vt(18.0), r.right() - x);
        text(p, pos2(x, y), Align2::LEFT_CENTER, &label, vt(18.0), color, 0.0);
    }
}

fn status_look(s: &ServiceStatus) -> (Color32, String) {
    match s.level {
        StatusLevel::Green => (theme::OK.fg, "OPERATIONAL".into()),
        StatusLevel::Yellow => (chrome::AMBER, if s.text.is_empty() { "DEGRADED".into() } else { s.text.to_uppercase() }),
        StatusLevel::Red => (theme::CRIT.fg, if s.text.is_empty() { "OUTAGE".into() } else { s.text.to_uppercase() }),
        StatusLevel::Unknown => (theme::NO_DATA.fg, "--".into()),
    }
}

/// Claude Desktop runs all its sessions in one process, so its registry
/// entry stands for every Desktop session (named after the latest one).
/// The project a session runs in: the last folder of its working directory,
/// else its name.
fn project_of(s: &Session) -> String {
    s.cwd
        .as_deref()
        .and_then(|c| std::path::Path::new(c.trim_end_matches(['\\', '/'])).file_name())
        .and_then(|f| f.to_str())
        .map_or_else(|| s.name.clone(), str::to_owned)
}

fn is_desktop(s: &Session) -> bool {
    s.entrypoint.as_deref().is_some_and(|e| e.to_lowercase().contains("desktop"))
}

/// Right-hand text of a session row: BUSY, what it waits for, IDLE or CLI.
fn session_meta(s: &Session) -> String {
    match &s.status {
        SessionStatus::Working => "BUSY".into(),
        SessionStatus::Waiting(Some(what)) if !what.is_empty() => what.to_uppercase(),
        SessionStatus::Waiting(_) => "WAITING".into(),
        // The Desktop row already says DESKTOP in its name.
        SessionStatus::Idle => if is_desktop(s) { "DESKTOP" } else { "CLI" }.into(),
    }
}

/// Hover text for a Claude Desktop row: why it shows only a project.
const DESKTOP_NOTE: &str = "Claude Desktop runs all of its Claude Code sessions in one process and writes a single entry for them in ~/.claude/sessions, so this row stands for every Desktop session. It shows the project of the most recent one; the conversation name in that entry is often stale, so it isn't shown. Sessions started from a terminal get one row each.";

/// Draws the rows; returns the rects of Claude Desktop rows (for hover help).
fn sessions_screen(p: &Painter, r: Rect, state: &AppState, tz: FixedOffset, blink_on: bool) -> Vec<Rect> {
    header(p, r, "CAM 04 · SESSIONS", &clock_of(state.sessions.last_success, tz));
    let list = &state.sessions.list;
    // Rows are 22 high, 4 apart, below the header; they may use the screen's padding.
    let top = r.top() + 16.0 + 4.0;
    let limit = r.bottom() + 6.0;
    let mut max = 0;
    while top + (max + 1) as f32 * 26.0 - 4.0 <= limit {
        max += 1;
    }
    let (shown, more) = if list.len() > max { (max.saturating_sub(1), list.len() - max.saturating_sub(1)) } else { (list.len(), 0) };
    let mut desktop = Vec::new();
    for (i, s) in list.iter().take(shown).enumerate() {
        let row = Rect::from_min_size(pos2(r.left() - 4.0, top + i as f32 * 26.0), vec2(r.width() + 8.0, 22.0));
        session_row(p, row, s, blink_on);
        if is_desktop(s) {
            desktop.push(row);
        }
    }
    if more > 0 && max > 0 {
        let y = top + shown as f32 * 26.0 + 11.0;
        text(p, pos2(r.left() + 50.0 + 8.0, y), Align2::LEFT_CENTER, &format!("+{more} MORE"), vt(16.0), theme::OK.dim, 0.0);
    }
    desktop
}

fn session_row(p: &Painter, row: Rect, s: &Session, blink_on: bool) {
    let waiting = matches!(s.status, SessionStatus::Waiting(_));
    if waiting && blink_on {
        p.rect_filled(row, 3.0, Color32::from_rgba_unmultiplied(251, 191, 36, 41));
    }
    let (tag, tag_fg, name_fg, meta_fg) = match &s.status {
        SessionStatus::Working => ("● REC", if blink_on { chrome::REC_ON } else { chrome::REC_OFF }, chrome::INK, theme::OK.fg),
        SessionStatus::Waiting(_) => ("◆ WAIT", chrome::AMBER, chrome::AMBER, chrome::AMBER),
        SessionStatus::Idle => ("○ IDLE", theme::OK.dim, chrome::PLATE_TEXT, theme::OK.dim),
    };
    let mid = row.center().y;
    let (x0, x1) = (row.left() + 4.0, row.right() - 4.0);
    text(p, pos2(x0, mid), Align2::LEFT_CENTER, tag, vt(17.0), tag_fg, 0.0);
    let meta = text(p, pos2(x1, mid), Align2::RIGHT_CENTER, &session_meta(s), vt(16.0), meta_fg, 0.0);
    let name_x = x0 + 50.0 + 8.0;
    let room = meta.left() - 8.0 - name_x;
    // "MAJIHANAYA · Plan review": the project (the session's folder) first,
    // the conversation name dimmed after it. Claude Desktop rows show the
    // project only: its single shared entry's name is often stale (see
    // DESKTOP_NOTE, shown on hover).
    let project = project_of(s);
    let lead_text = fit(p, &project.to_uppercase(), &vt(17.0), room);
    let lead = text(p, pos2(name_x, mid), Align2::LEFT_CENTER, &lead_text, vt(17.0), name_fg, 1.0);
    if !is_desktop(s) && !s.name.eq_ignore_ascii_case(&project) {
        let x = lead.right() + 6.0;
        let rest = fit(p, &format!("· {}", s.name), &mono(11.0), meta.left() - 8.0 - x);
        text(p, pos2(x, mid), Align2::LEFT_CENTER, &rest, mono(11.0), chrome::MUTED, 0.0);
    }
}

struct ClockRow {
    label: String,
    at: String,
    left: String,
    ph: Phosphor,
}

/// One row per usage window that has a reset time.
fn clock_rows(state: &AppState, config: &Config, now: DateTime<Utc>, tz: FixedOffset) -> Vec<ClockRow> {
    let mut rows = Vec::new();
    let mut push = |label: String, percent: f32, resets_at: Option<DateTime<Utc>>| {
        if let Some(t) = resets_at {
            rows.push(ClockRow {
                label,
                at: t.with_timezone(&tz).format("%a %H:%M").to_string().to_uppercase(),
                left: left(t, now),
                ph: phosphor_for(percent, config),
            });
        }
    };
    if config.show_claude {
        let l = &state.claude.limits;
        if let Some(w) = &l.session {
            push("CLAUDE 5H".into(), w.percent as f32, w.resets_at);
        }
        if let Some(w) = &l.week {
            push("CLAUDE WEEK".into(), w.percent as f32, w.resets_at);
        }
        for (name, w) in &l.model_weeks {
            push(format!("{} WEEK", name.to_uppercase()), w.percent as f32, w.resets_at);
        }
    }
    if config.show_codex {
        for w in &state.codex.windows {
            let title = if w.label == "5-hour" { "5H".to_string() } else { w.label.to_uppercase() };
            push(format!("CODEX {title}"), w.percent, w.resets_at);
        }
    }
    rows
}

/// "UTC+7", "UTC+5:30", "UTC-3".
fn utc_label(tz: FixedOffset) -> String {
    let mins = tz.local_minus_utc() / 60;
    let sign = if mins < 0 { '-' } else { '+' };
    let (h, m) = (mins.abs() / 60, mins.abs() % 60);
    if m == 0 { format!("UTC{sign}{h}") } else { format!("UTC{sign}{h}:{m:02}") }
}

fn clocks_screen(p: &Painter, r: Rect, rows: &[ClockRow], tz: FixedOffset) {
    header(p, r, "CAM 05 · RESET CLOCKS", &utc_label(tz));
    for (i, k) in rows.iter().take(4).enumerate() {
        let mid = r.top() + 16.0 + 2.0 + i as f32 * 18.0 + 8.0;
        text(p, pos2(r.left(), mid), Align2::LEFT_CENTER, &k.label, vt(18.0), k.ph.fg, 0.0);
        text(p, pos2(r.right() - 152.0, mid), Align2::LEFT_CENTER, &k.at, vt(18.0), k.ph.fg, 0.0);
        text(p, pos2(r.right(), mid), Align2::RIGHT_CENTER, &k.left, vt(18.0), k.ph.dim, 0.0);
    }
    text(p, pos2(r.left(), r.bottom()), Align2::LEFT_BOTTOM, "local time", mono(10.0), theme::OK.dim, 0.0);
}

/// 1874 -> "1,874".
fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn traffic_screen(p: &Painter, r: Rect, state: &AppState, config: &Config) {
    header(p, r, "CAM 06 · TRAFFIC", "CLAUDE");
    let l = &state.claude.limits;
    let blocks: Vec<(&str, &UsageStats)> = if config.show_stats {
        [("24H", l.last_24h.as_ref()), ("7D", l.last_7d.as_ref())].into_iter().filter_map(|(s, u)| u.map(|u| (s, u))).collect()
    } else {
        Vec::new()
    };
    if blocks.is_empty() {
        text(p, r.center() + vec2(0.0, 8.0), Align2::CENTER_CENTER, "NO STATS", vt(18.0), theme::NO_DATA.fg, 0.0);
        return;
    }
    let ok = theme::OK;
    let mut y = r.top() + 16.0 + 4.0;
    for (span, u) in blocks {
        let base = y + 24.0;
        text(p, pos2(r.left(), base), Align2::LEFT_BOTTOM, span, vt(18.0), ok.dim, 0.0);
        let req = thousands(u.requests);
        let pos = pos2(r.left() + 34.0 + 8.0, base + 4.0);
        let halo = ok.fg.gamma_multiply(0.22);
        for d in [vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0)] {
            text(p, pos + d, Align2::LEFT_BOTTOM, &req, vt(30.0), halo, 0.0);
        }
        let rr = text(p, pos, Align2::LEFT_BOTTOM, &req, vt(30.0), ok.fg, 0.0);
        text(p, pos2(rr.right() + 8.0, base), Align2::LEFT_BOTTOM, "REQ", vt(17.0), ok.dim, 0.0);
        let sess = format!("{} SESSION{}", u.sessions, if u.sessions == 1 { "" } else { "S" });
        text(p, pos2(r.right(), base), Align2::RIGHT_BOTTOM, &sess, vt(18.0), ok.fg, 0.0);
        y += 26.0 + 1.0;
        for line in &u.lines {
            if y + 14.0 > r.bottom() + 6.0 {
                break;
            }
            let s = fit(p, line, &mono(10.0), r.width() - 42.0);
            text(p, pos2(r.left() + 42.0, y + 7.0), Align2::LEFT_CENTER, &s, mono(10.0), ok.dim, 0.0);
            y += 14.0;
        }
        y += 4.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_and_zone_labels() {
        assert_eq!(thousands(312), "312");
        assert_eq!(thousands(1874), "1,874");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(utc_label(FixedOffset::east_opt(7 * 3600).unwrap()), "UTC+7");
        assert_eq!(utc_label(FixedOffset::east_opt(5 * 3600 + 1800).unwrap()), "UTC+5:30");
        assert_eq!(utc_label(FixedOffset::west_opt(3 * 3600).unwrap()), "UTC-3");
    }

    #[test]
    fn session_meta_text() {
        let s = |status, entrypoint: Option<&str>| Session { pid: 1, name: "x".into(), status, cwd: None, entrypoint: entrypoint.map(Into::into) };
        assert_eq!(session_meta(&s(SessionStatus::Working, None)), "BUSY");
        assert_eq!(session_meta(&s(SessionStatus::Waiting(Some("permission".into())), None)), "PERMISSION");
        assert_eq!(session_meta(&s(SessionStatus::Idle, Some("claude-desktop"))), "DESKTOP");
        assert_eq!(session_meta(&s(SessionStatus::Idle, None)), "CLI");
    }

    #[test]
    fn project_from_cwd() {
        let s = |cwd: Option<&str>| Session { pid: 1, name: "Plan review".into(), status: SessionStatus::Idle, cwd: cwd.map(Into::into), entrypoint: None };
        assert_eq!(project_of(&s(Some(r"C:\Users\me\Lab\majihanaya"))), "majihanaya");
        assert_eq!(project_of(&s(Some(r"C:\Users\me\Lab\majihanaya\"))), "majihanaya");
        assert_eq!(project_of(&s(None)), "Plan review");
    }
}
