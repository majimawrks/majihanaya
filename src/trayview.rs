//! What the tray shows, as pure functions of `AppState` (no Win32 here):
//! which number, its level, the icon bitmap and the tooltip text.
//! Bitmaps follow the design's Tray board: a tiny monitor with 3×5 digits.

use crate::config::{Config, TrayNumber};
use crate::model::*;
use chrono::{DateTime, Local, Utc};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Crit,
    NoData,
}

impl Level {
    pub fn of(percent: f32, config: &Config) -> Self {
        if percent >= config.crit_percent as f32 {
            Level::Crit
        } else if percent >= config.warn_percent as f32 {
            Level::Warn
        } else {
            Level::Ok
        }
    }

    /// The plan's tray colours (design Spec board, "fg").
    pub fn rgb(self) -> u32 {
        match self {
            Level::Ok => 0x4ADE80,
            Level::Warn => 0xFBBF24,
            Level::Crit => 0xF87171,
            Level::NoData => 0x6B7280,
        }
    }
}

/// One usage window as the tray sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// "Claude 5h", "Claude week", "Fable week", "Codex week", …
    pub label: String,
    pub percent: f32,
    pub resets_at: Option<DateTime<Utc>>,
    /// Shown, but not eligible to drive the icon.
    pub stale: bool,
    /// When the stale data was last fresh, for "(stale 11:10)".
    pub stale_since: Option<DateTime<Utc>>,
    pub claude_session: bool,
    pub claude_week: bool,
    pub codex: bool,
}

/// Everything the tray needs, derived once per state change.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// "--" when nothing eligible; else the rounded percent ("100" at the limit).
    pub text: String,
    pub level: Level,
    /// Index into `lines` of the window driving the icon.
    pub driver: Option<usize>,
    pub lines: Vec<Line>,
    /// One-line notices, e.g. "Claude: not logged in".
    pub notices: Vec<String>,
}

fn claude_short(label: &str) -> String {
    format!("{label} week")
}

fn codex_short(label: &str) -> String {
    match label {
        "5-hour" => "Codex 5h".into(),
        other => format!("Codex {other}"),
    }
}

pub fn view(state: &AppState, config: &Config, now: DateTime<Utc>) -> View {
    let mut lines = Vec::new();
    let mut notices = Vec::new();

    let c = &state.claude;
    if config.show_claude && c.presence != Presence::Missing {
        let stale = is_stale(c.last_success, Duration::from_secs(config.usage_interval_seconds), now);
        let stale_since = c.last_success.filter(|_| stale);
        if c.org_mismatch {
            notices.push("Claude: two different accounts".into());
        } else if c.source != ClaudeSource::None {
            let mut push = |label: String, l: &Limit, session: bool, week: bool| {
                lines.push(Line {
                    label,
                    percent: l.percent as f32,
                    resets_at: l.resets_at,
                    stale,
                    stale_since,
                    claude_session: session,
                    claude_week: week,
                    codex: false,
                });
            };
            if let Some(l) = &c.limits.session {
                push("Claude 5h".into(), l, true, false);
            }
            if let Some(l) = &c.limits.week {
                push("Claude week".into(), l, false, true);
            }
            for (name, l) in &c.limits.model_weeks {
                push(claude_short(name), l, false, false);
            }
        }
        if c.desktop_only {
            notices.push("Claude: full usage needs Claude Code".into());
        } else if c.account.as_ref().is_some_and(|a| !a.logged_in) {
            notices.push("Claude: not logged in".into());
        }
    }

    let x = &state.codex;
    if config.show_codex && x.presence != Presence::Missing {
        // Rollout data is historical and carries no account (it may be a
        // previous login's): shown as stale, never driving the icon.
        let stale = x.source == CodexSource::Rollout
            || is_stale(x.last_success, Duration::from_secs(config.codex_interval_seconds), now);
        let stale_since = x.last_success.filter(|_| stale);
        for w in &x.windows {
            lines.push(Line {
                label: codex_short(&w.label),
                percent: w.percent,
                resets_at: w.resets_at,
                stale,
                stale_since,
                claude_session: false,
                claude_week: false,
                codex: true,
            });
        }
    }

    let eligible = |l: &Line| {
        !l.stale
            && match config.tray_number {
                TrayNumber::Worst => true,
                TrayNumber::ClaudeSession => l.claude_session,
                TrayNumber::ClaudeWeek => l.claude_week,
                TrayNumber::Codex => l.codex,
            }
    };
    // Highest percent wins; on a tie the earlier line (Claude 5h first).
    let mut driver: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if eligible(l) && driver.is_none_or(|d| l.percent > lines[d].percent) {
            driver = Some(i);
        }
    }

    let (text, level) = match driver {
        Some(d) => {
            let p = lines[d].percent.clamp(0.0, 100.0);
            (format!("{}", p.round() as u32), Level::of(p, config))
        }
        None => ("--".into(), Level::NoData),
    };
    View { text, level, driver, lines, notices }
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

/// Windows keeps 127 UTF-16 units of a tray tooltip (NOTIFYICONDATAW.szTip).
pub const TOOLTIP_MAX: usize = 127;

/// "42m", "3h58m", "4d20h".
pub fn countdown(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (until - now).num_minutes().max(0);
    let (d, h, m) = (mins / 1440, mins / 60 % 24, mins % 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

fn line_text(l: &Line, driver: bool, clock: bool, now: DateTime<Utc>) -> String {
    let mark = if driver { "▸" } else { "" };
    let pct = format!("{}%", l.percent.clamp(0.0, 100.0).round() as u32);
    let tail = match (l.stale, l.stale_since, l.resets_at) {
        (true, Some(t), _) => format!(" (stale {})", t.with_timezone(&Local).format("%H:%M")),
        (true, None, _) => " (stale)".into(),
        (false, _, Some(r)) if clock => format!(" · {}", r.with_timezone(&Local).format("%a %H:%M")),
        (false, _, Some(r)) => format!(" · {}", countdown(r, now)),
        (false, _, None) => String::new(),
    };
    format!("{mark}{} {pct}{tail}", l.label)
}

/// Design: title line, one line per window, the icon's driver marked ▸.
/// Lines that don't fit are dropped from the end, never cut mid-line.
pub fn tooltip(v: &View, clock: bool, now: DateTime<Utc>) -> String {
    let mut out = String::from("majihanaya");
    let rows = v
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| line_text(l, v.driver == Some(i), clock, now))
        .chain(v.notices.iter().cloned());
    for row in rows {
        let next = format!("{out}\n{row}");
        if next.encode_utf16().count() > TOOLTIP_MAX {
            break;
        }
        out = next;
    }
    out
}

// ---------------------------------------------------------------------------
// Icon bitmap
// ---------------------------------------------------------------------------

const RIM: u32 = 0x4A4D45;
const SCREEN: u32 = 0x050605;

/// 3×5 digits; '1' is two columns so it never reads as '7'.
fn glyph(c: char) -> &'static [&'static str; 5] {
    match c {
        '0' => &["111", "101", "101", "101", "111"],
        '1' => &["01", "11", "01", "01", "01"],
        '2' => &["111", "001", "111", "100", "111"],
        '3' => &["111", "001", "111", "001", "111"],
        '4' => &["101", "101", "111", "001", "001"],
        '5' => &["111", "100", "111", "001", "111"],
        '6' => &["111", "100", "111", "101", "111"],
        '7' => &["111", "001", "001", "001", "001"],
        '8' => &["111", "101", "111", "101", "111"],
        '9' => &["111", "101", "111", "001", "111"],
        _ => &["000", "000", "111", "000", "000"], // '-'
    }
}

struct Canvas {
    size: u32,
    rgba: Vec<u8>,
}

impl Canvas {
    fn new(size: u32) -> Self {
        Self { size, rgba: vec![0; (size * size * 4) as usize] }
    }

    fn rect(&mut self, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
        for yy in y..(y + h).min(self.size) {
            for xx in x..(x + w).min(self.size) {
                let i = ((yy * self.size + xx) * 4) as usize;
                self.rgba[i..i + 4].copy_from_slice(&[(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]);
            }
        }
    }

    fn text_width(text: &str, sx: u32, gap: u32) -> u32 {
        let cols: u32 = text.chars().map(|c| glyph(c)[0].len() as u32).sum();
        cols * sx + (text.chars().count() as u32).saturating_sub(1) * gap
    }

    fn digits(&mut self, text: &str, x0: u32, y0: u32, sx: u32, sy: u32, gap: u32, rgb: u32) {
        let mut ox = x0;
        for c in text.chars() {
            let g = glyph(c);
            for (row, bits) in g.iter().enumerate() {
                for (col, b) in bits.bytes().enumerate() {
                    if b == b'1' {
                        self.rect(ox + col as u32 * sx, y0 + row as u32 * sy, sx, sy, rgb);
                    }
                }
            }
            ox += g[0].len() as u32 * sx + gap;
        }
    }
}

/// RGBA for a `size`×`size` icon (16 or 32; anything else uses 32's drawing
/// scaled by the shell).
pub fn icon_rgba(text: &str, level: Level, size: u32) -> (Vec<u8>, u32) {
    let ink = level.rgb();
    if size <= 16 {
        let mut c = Canvas::new(16);
        c.rect(1, 0, 14, 13, RIM);
        c.rect(0, 1, 16, 11, RIM);
        c.rect(1, 1, 14, 11, SCREEN);
        c.rect(6, 13, 4, 1, RIM);
        c.rect(4, 14, 8, 1, RIM);
        if text.chars().count() > 2 {
            // "100" can't fit at 16 px: the screen lights solid instead.
            c.rect(1, 1, 14, 11, ink);
        } else {
            let w = Canvas::text_width(text, 2, 2);
            c.digits(text, 1 + 14u32.saturating_sub(w) / 2, 1, 2, 2, 2, ink);
        }
        (c.rgba, 16)
    } else {
        let mut c = Canvas::new(32);
        c.rect(1, 1, 30, 25, RIM);
        c.rect(0, 2, 32, 23, RIM);
        c.rect(2, 3, 28, 21, SCREEN);
        c.rect(13, 26, 6, 2, RIM);
        c.rect(9, 28, 14, 2, RIM);
        let two = text.chars().count() <= 2;
        let (sx, gap) = if two { (3, 3) } else { (2, 2) };
        let w = Canvas::text_width(text, sx, gap);
        c.digits(text, 2 + 28u32.saturating_sub(w) / 2, 6, sx, 3, gap, ink);
        (c.rgba, 32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 18, h, m, 0).unwrap()
    }

    fn state_with(now: DateTime<Utc>, session: u8, week: u8, codex: f32) -> AppState {
        let mut s = AppState::default();
        s.claude.presence = Presence::Found;
        s.claude.source = ClaudeSource::Cli;
        s.claude.last_success = Some(now);
        s.claude.limits.session = Some(Limit { percent: session, resets_at: Some(now + chrono::Duration::minutes(238)) });
        s.claude.limits.week = Some(Limit { percent: week, resets_at: None });
        s.claude.limits.model_weeks = vec![("Fable".into(), Limit { percent: 18, resets_at: None })];
        s.codex.presence = Presence::Found;
        s.codex.last_success = Some(now);
        s.codex.windows = vec![CodexWindow { label: "week".into(), bucket: None, window_mins: Some(10080), percent: codex, resets_at: None }];
        s
    }

    #[test]
    fn worst_window_drives_icon_with_claude_first_on_tie() {
        let now = at(12, 0);
        let cfg = Config::default();
        let v = view(&state_with(now, 42, 54, 38.0), &cfg, now);
        assert_eq!((v.text.as_str(), v.level), ("54", Level::Ok));
        assert_eq!(v.lines[v.driver.unwrap()].label, "Claude week");
        let v = view(&state_with(now, 70, 70, 70.0), &cfg, now);
        assert_eq!(v.lines[v.driver.unwrap()].label, "Claude 5h");
        assert_eq!(v.level, Level::Warn);
    }

    #[test]
    fn modes_select_their_window() {
        let now = at(12, 0);
        let s = state_with(now, 42, 54, 90.0);
        let mut cfg = Config::default();
        cfg.tray_number = TrayNumber::ClaudeSession;
        assert_eq!(view(&s, &cfg, now).text, "42");
        cfg.tray_number = TrayNumber::Codex;
        let v = view(&s, &cfg, now);
        assert_eq!((v.text.as_str(), v.level), ("90", Level::Crit));
    }

    #[test]
    fn stale_feeds_are_skipped_and_nothing_live_is_no_data() {
        let now = at(12, 0);
        let mut s = state_with(now, 42, 54, 63.0);
        s.claude.last_success = Some(now - chrono::Duration::minutes(10));
        let v = view(&s, &Config::default(), now);
        assert_eq!(v.text, "63");
        s.codex.last_success = None;
        let v = view(&s, &Config::default(), now);
        assert_eq!((v.text.as_str(), v.level), ("--", Level::NoData));
        assert!(tooltip(&v, false, now).contains("(stale"));
    }

    #[test]
    fn tooltip_matches_design_and_fits() {
        let now = at(12, 0);
        let v = view(&state_with(now, 42, 54, 38.0), &Config::default(), now);
        let t = tooltip(&v, false, now);
        assert_eq!(t, "majihanaya\nClaude 5h 42% · 3h58m\n▸Claude week 54%\nFable week 18%\nCodex week 38%");
        let mut long = v.clone();
        long.lines = (0..20).map(|i| Line { label: format!("Window {i}"), ..v.lines[0].clone() }).collect();
        let t = tooltip(&long, false, now);
        assert!(t.encode_utf16().count() <= TOOLTIP_MAX);
        assert!(t.ends_with('m'), "no line is cut: {t}");
    }

    #[test]
    fn countdown_formats() {
        let now = at(12, 0);
        assert_eq!(countdown(now + chrono::Duration::minutes(42), now), "42m");
        assert_eq!(countdown(now + chrono::Duration::minutes(238), now), "3h58m");
        assert_eq!(countdown(now + chrono::Duration::minutes(6960), now), "4d20h");
        assert_eq!(countdown(now - chrono::Duration::minutes(5), now), "0m");
    }

    #[test]
    fn icon_sizes_and_limit_state() {
        for size in [16, 32] {
            for text in ["--", "7", "54", "100"] {
                let (rgba, s) = icon_rgba(text, Level::Ok, size);
                assert_eq!(rgba.len(), (s * s * 4) as usize);
            }
        }
        // 16 px "100": screen lit solid in the level colour.
        let (rgba, _) = icon_rgba("100", Level::Crit, 16);
        let i = ((5 * 16 + 7) * 4) as usize;
        assert_eq!(&rgba[i..i + 3], &[0xF8, 0x71, 0x71]);
    }
}

#[cfg(test)]
mod preview {
    /// Writes raw RGBA icons for eyeballing: `MJH_PREVIEW=<dir> cargo test tray_preview -- --ignored`.
    #[test]
    #[ignore]
    fn tray_preview() {
        let dir = std::env::var("MJH_PREVIEW").expect("MJH_PREVIEW dir");
        for (text, level) in [("54", super::Level::Ok), ("71", super::Level::Warn), ("93", super::Level::Crit), ("100", super::Level::Crit), ("--", super::Level::NoData), ("7", super::Level::Ok)] {
            for size in [16, 32] {
                let (rgba, s) = super::icon_rgba(text, level, size);
                std::fs::write(format!("{dir}/{text}_{s}.rgba"), rgba).unwrap();
            }
        }
    }
}
