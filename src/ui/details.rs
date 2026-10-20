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
                status_screen(p, r, state, config, now, tz);
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
            if let Some(n) = &s.notice {
                items.push(monitor::notice_height(n));
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

/// More characters than any screen line can show: longer text is cut to
/// this before measuring, so a huge name costs no more than a long one.
const FIT_MAX_CHARS: usize = 256;

/// `s` shortened with an ellipsis so it fits `max_w`.
fn fit(p: &Painter, s: &str, font: &FontId, max_w: f32) -> String {
    let width = |t: &str| p.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x;
    fit_with(s, max_w, width)
}

/// `fit` with the measuring done by `width`: a binary search for the
/// longest prefix that fits with its ellipsis (a few layouts, not one per
/// character).
fn fit_with(s: &str, max_w: f32, width: impl Fn(&str) -> f32) -> String {
    let chars: Vec<char> = s.chars().take(FIT_MAX_CHARS + 1).collect();
    if chars.len() <= FIT_MAX_CHARS && width(s) <= max_w {
        return s.to_string();
    }
    let cut = |n: usize| chars[..n].iter().collect::<String>() + "…";
    // The longest prefix that fits is in lo..=hi; "…" alone is the floor.
    let (mut lo, mut hi) = (0, chars.len().min(FIT_MAX_CHARS));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max_w { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

fn status_screen(p: &Painter, r: Rect, state: &AppState, config: &Config, now: DateTime<Utc>, tz: FixedOffset) {
    header(p, r, "CAM 07 · STATUS", &clock_of(state.claude_status.last_success, tz));
    let interval = std::time::Duration::from_secs(config.status_interval_seconds);
    let rows = [("CLAUDE", &state.claude_status), ("CODEX", &state.codex_status)];
    for (i, (name, status)) in rows.into_iter().enumerate() {
        let y = r.top() + 16.0 + 3.0 + i as f32 * 19.0 + 8.0;
        let (color, label) = status_look(status, status.level_at(interval, now));
        p.circle_filled(pos2(r.left() + 3.5, y), 3.5, color);
        text(p, pos2(r.left() + 15.0, y), Align2::LEFT_CENTER, name, vt(18.0), color, 0.0);
        let x = r.left() + 15.0 + 56.0 + 8.0;
        let label = fit(p, &label, &vt(18.0), r.right() - x);
        text(p, pos2(x, y), Align2::LEFT_CENTER, &label, vt(18.0), color, 0.0);
    }
}

/// `level`: the status's level as of now (`Unknown` once stale).
fn status_look(s: &ServiceStatus, level: StatusLevel) -> (Color32, String) {
    match level {
        // Had a good fetch once, but nothing recent: say so rather than "--".
        StatusLevel::Unknown if s.last_success.is_some() => (theme::NO_DATA.fg, "UNAVAILABLE".into()),
        StatusLevel::Green => (theme::OK.fg, "OPERATIONAL".into()),
        StatusLevel::Yellow => (chrome::AMBER, if s.text.is_empty() { "DEGRADED".into() } else { s.text.to_uppercase() }),
        StatusLevel::Red => (theme::CRIT.fg, if s.text.is_empty() { "OUTAGE".into() } else { s.text.to_uppercase() }),
        StatusLevel::Unknown => (theme::NO_DATA.fg, "--".into()),
    }
}

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

/// Left column of a session row: what the session is doing.
fn session_tag(s: &Session) -> &'static str {
    match s.status {
        SessionStatus::Working => "● BUSY",
        SessionStatus::Waiting(_) => "◆ WAIT",
        SessionStatus::Idle => "○ IDLE",
    }
}

/// Right column: what a waiting session waits for; nothing otherwise.
fn session_detail(s: &Session) -> Option<String> {
    match &s.status {
        SessionStatus::Waiting(Some(what)) if !what.is_empty() => Some(what.to_uppercase()),
        _ => None,
    }
}

/// Hover text for a Claude Desktop row: why its title may be out of date.
const DESKTOP_NOTE: &str = "Claude Desktop runs all of its Claude Code sessions in one process and writes a single entry for them in ~/.claude/sessions, so this row stands for every Desktop session. The title shown is the one in that entry: Desktop doesn't update it as you switch or rename conversations, so it is often stale. Sessions started from a terminal get one row each, with live titles.";

/// Marquee speed for a hovered, cut-off title, in points per second.
const MARQUEE_SPEED: f32 = 30.0;
/// Blank space between the end of a scrolling title and its next pass.
const MARQUEE_GAP: f32 = 32.0;

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
    let hover = p.ctx().pointer_hover_pos();
    let mut desktop = Vec::new();
    for (i, s) in list.iter().take(shown).enumerate() {
        let row = Rect::from_min_size(pos2(r.left() - 4.0, top + i as f32 * 26.0), vec2(r.width() + 8.0, 22.0));
        session_row(p, row, s, blink_on, hover.is_some_and(|h| row.contains(h)));
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

/// A 12×10 glyph for where the session runs: a monitor on a stand for
/// Claude Desktop, a `>_` prompt for the CLI.
fn kind_glyph(p: &Painter, left: f32, mid: f32, desktop: bool, color: Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    if desktop {
        p.rect_stroke(Rect::from_min_max(pos2(left, mid - 5.0), pos2(left + 12.0, mid + 2.5)), 1.0, stroke, egui::StrokeKind::Inside);
        let cx = left + 6.0;
        p.line_segment([pos2(cx, mid + 2.5), pos2(cx, mid + 4.5)], stroke);
        p.line_segment([pos2(cx - 3.5, mid + 5.0), pos2(cx + 3.5, mid + 5.0)], stroke);
    } else {
        p.line_segment([pos2(left + 0.5, mid - 4.0), pos2(left + 4.5, mid)], stroke);
        p.line_segment([pos2(left + 4.5, mid), pos2(left + 0.5, mid + 4.0)], stroke);
        p.line_segment([pos2(left + 6.0, mid + 4.5), pos2(left + 12.0, mid + 4.5)], stroke);
    }
}

/// Status | kind glyph · PROJECT · title | waiting reason.
fn session_row(p: &Painter, row: Rect, s: &Session, blink_on: bool, hovered: bool) {
    let waiting = matches!(s.status, SessionStatus::Waiting(_));
    if waiting && blink_on {
        p.rect_filled(row, 3.0, Color32::from_rgba_unmultiplied(251, 191, 36, 41));
    }
    let (tag_fg, name_fg, glyph_fg) = match &s.status {
        SessionStatus::Working => (if blink_on { chrome::REC_ON } else { chrome::REC_OFF }, chrome::INK, theme::OK.fg),
        SessionStatus::Waiting(_) => (chrome::AMBER, chrome::AMBER, chrome::AMBER),
        SessionStatus::Idle => (theme::OK.dim, chrome::PLATE_TEXT, theme::OK.dim),
    };
    let mid = row.center().y;
    let (x0, x1) = (row.left() + 4.0, row.right() - 4.0);
    text(p, pos2(x0, mid), Align2::LEFT_CENTER, session_tag(s), vt(17.0), tag_fg, 0.0);
    let right = match session_detail(s) {
        Some(d) => text(p, pos2(x1, mid), Align2::RIGHT_CENTER, &d, vt(16.0), chrome::AMBER, 0.0).left() - 8.0,
        None => x1,
    };
    let glyph_x = x0 + 50.0 + 8.0;
    kind_glyph(p, glyph_x, mid, is_desktop(s), glyph_fg);
    let name_x = glyph_x + 12.0 + 6.0;
    // "MAJIHANAYA · Plan review": the project (the session's folder) first,
    // the conversation title dimmed after it.
    let project = project_of(s);
    let lead_text = fit(p, &project.to_uppercase(), &vt(17.0), right - name_x);
    let lead = text(p, pos2(name_x, mid), Align2::LEFT_CENTER, &lead_text, vt(17.0), name_fg, 1.0);
    if s.name.eq_ignore_ascii_case(&project) {
        return;
    }
    let dot = text(p, pos2(lead.right() + 6.0, mid), Align2::LEFT_CENTER, "·", mono(11.0), chrome::MUTED, 0.0);
    let area = Rect::from_min_max(pos2(dot.right() + 4.0, row.top()), pos2(right, row.bottom()));
    if area.width() <= 8.0 {
        return;
    }
    title_text(p, area, &s.name, hovered);
}

/// The dimmed title in `area`: cut with an ellipsis, or, while the row is
/// hovered and the title doesn't fit, scrolling through it.
fn title_text(p: &Painter, area: Rect, title: &str, hovered: bool) {
    let font = mono(11.0);
    let mid = area.center().y;
    let title: String = title.chars().take(FIT_MAX_CHARS).collect();
    let galley = p.layout_no_wrap(title.clone(), font.clone(), chrome::MUTED);
    let w = galley.size().x;
    if !hovered || w <= area.width() {
        let cut = fit(p, &title, &font, area.width());
        text(p, pos2(area.left(), mid), Align2::LEFT_CENTER, &cut, font, chrome::MUTED, 0.0);
        return;
    }
    let ctx = p.ctx();
    let period = w + MARQUEE_GAP;
    let offset = (ctx.input(|i| i.time) as f32 * MARQUEE_SPEED) % period;
    let clipped = p.with_clip_rect(area.intersect(p.clip_rect()));
    let y = mid - galley.size().y / 2.0;
    clipped.galley(pos2(area.left() - offset, y), galley.clone(), chrome::MUTED);
    clipped.galley(pos2(area.left() - offset + period, y), galley, chrome::MUTED);
    // Only while the pointer is on this row; leaving it stops the motion.
    ctx.request_repaint();
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
    // A fourth row takes the note's place (the header already names the zone).
    if rows.len() < 4 {
        text(p, pos2(r.left(), r.bottom()), Align2::LEFT_BOTTOM, "local time", mono(10.0), theme::OK.dim, 0.0);
    }
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
    fn fit_cuts_long_text_with_few_measurements() {
        let calls = std::cell::Cell::new(0);
        // One unit per character.
        let width = |t: &str| {
            calls.set(calls.get() + 1);
            t.chars().count() as f32
        };
        assert_eq!(fit_with("short", 10.0, width), "short");
        assert_eq!(fit_with("abcdefghijkl", 6.0, width), "abcde…");
        assert_eq!(fit_with("abc", 0.5, width), "…");
        calls.set(0);
        let huge = "x".repeat(1_000_000);
        assert_eq!(fit_with(&huge, 11.0, width), format!("{}…", "x".repeat(10)));
        assert!(calls.get() < 20, "{} measurements", calls.get());
    }

    #[test]
    fn session_columns() {
        let s = |status, entrypoint: Option<&str>| Session { pid: 1, name: "x".into(), status, cwd: None, entrypoint: entrypoint.map(Into::into) };
        // Left: always the status; right: only what a waiting session waits for.
        assert_eq!(session_tag(&s(SessionStatus::Working, None)), "● BUSY");
        assert_eq!(session_tag(&s(SessionStatus::Idle, Some("claude-desktop"))), "○ IDLE");
        assert_eq!(session_tag(&s(SessionStatus::Waiting(None), None)), "◆ WAIT");
        assert_eq!(session_detail(&s(SessionStatus::Waiting(Some("permission".into())), None)).as_deref(), Some("PERMISSION"));
        assert_eq!(session_detail(&s(SessionStatus::Waiting(None), None)), None);
        assert_eq!(session_detail(&s(SessionStatus::Idle, Some("claude-desktop"))), None);
        assert_eq!(session_detail(&s(SessionStatus::Working, None)), None);
        assert!(is_desktop(&s(SessionStatus::Idle, Some("claude-desktop"))));
        assert!(!is_desktop(&s(SessionStatus::Idle, None)));
    }

    #[test]
    fn project_from_cwd() {
        let s = |cwd: Option<&str>| Session { pid: 1, name: "Plan review".into(), status: SessionStatus::Idle, cwd: cwd.map(Into::into), entrypoint: None };
        assert_eq!(project_of(&s(Some(r"C:\Users\me\Lab\majihanaya"))), "majihanaya");
        assert_eq!(project_of(&s(Some(r"C:\Users\me\Lab\majihanaya\"))), "majihanaya");
        assert_eq!(project_of(&s(None)), "Plan review");
    }
}
