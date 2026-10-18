//! What each monitor shows, derived from `AppState` (no drawing here).
//! States follow the design boards: live (ok/warn/crit), AUX (Claude Desktop
//! fallback), NO SIGNAL (stale), FEED LOST, and ACQUIRING (no data yet).

use super::theme::{self, Phosphor};
use crate::config::Config;
use crate::model::*;
use chrono::{DateTime, FixedOffset, Utc};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Live,
    /// Claude Desktop history: numbers but no resets, stats or model weeks.
    Aux,
    /// Never had data yet.
    Acquiring,
    /// Old data, frozen: "NO SIGNAL · 11:10:35".
    Stale { frozen_at: String },
    /// No data and the source is failing.
    Lost { why: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Extra {
    pub label: String,
    pub percent: Option<f32>,
    pub ph: Phosphor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub cam: String,
    pub title: String,
    pub mode: Mode,
    pub ph: Phosphor,
    pub percent: Option<f32>,
    pub reset_a: String,
    pub reset_b: String,
    /// Top-right burn-in: the feed timestamp, or LIMIT / VIA DESKTOP / RETRYING.
    pub tag: String,
    pub tag_color: eframe::egui::Color32,
    pub extras: Vec<Extra>,
    /// Which source refreshes this screen ("claude" / "codex"): drives the sweep.
    pub source: &'static str,
}

impl Feed {
    /// Screen height (Spec: 82, +18 per per-model week line, +10 for the
    /// NO SIGNAL band).
    pub fn screen_height(&self) -> f32 {
        82.0 + 18.0 * self.extras.len() as f32 + if matches!(self.mode, Mode::Stale { .. }) { 10.0 } else { 0.0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub name: &'static str,
    pub plan: String,
    pub plan_color: eframe::egui::Color32,
    pub email: Option<String>,
    pub notice: Option<String>,
    pub feeds: Vec<Feed>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Footer {
    pub status: Option<(StatusLevel, String)>,
    /// `None` when sessions are hidden in settings.
    pub sessions: Option<usize>,
    pub busy: usize,
    pub waiting: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wall {
    pub sections: Vec<Section>,
    pub footer: Footer,
    /// Power LED: grey until anything has answered.
    pub powered: bool,
}

pub fn phosphor_for(percent: f32, config: &Config) -> Phosphor {
    if percent >= config.crit_percent as f32 {
        theme::CRIT
    } else if percent >= config.warn_percent as f32 {
        theme::WARN
    } else {
        theme::OK
    }
}

/// "3H 58M", "4D 20H", "42M".
pub fn left(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (until - now).num_minutes().max(0);
    let (d, h, m) = (mins / 1440, mins / 60 % 24, mins % 60);
    if d > 0 {
        format!("{d}D {h}H")
    } else if h > 0 {
        format!("{h}H {m}M")
    } else {
        format!("{m}M")
    }
}

/// "15:40" within a day, else "THU 08:00".
pub fn at(t: DateTime<Utc>, now: DateTime<Utc>, tz: FixedOffset) -> String {
    let local = t.with_timezone(&tz);
    if t - now < chrono::Duration::hours(20) {
        local.format("%H:%M").to_string()
    } else {
        local.format("%a %H:%M").to_string().to_uppercase()
    }
}

fn clock(t: Option<DateTime<Utc>>, tz: FixedOffset) -> String {
    t.map_or("--:--:--".into(), |t| t.with_timezone(&tz).format("%H:%M:%S").to_string())
}

struct Ctx<'a> {
    config: &'a Config,
    now: DateTime<Utc>,
    tz: FixedOffset,
}

impl Ctx<'_> {
    fn live_feed(&self, cam: &str, title: &str, l: Option<(f32, Option<DateTime<Utc>>)>, base: &Base) -> Feed {
        let source = base.source;
        let (percent, resets_at) = l.map_or((None, None), |(p, r)| (Some(p), r));
        let mut feed = Feed {
            cam: cam.into(),
            title: title.into(),
            mode: base.mode.clone(),
            ph: theme::NO_DATA,
            percent,
            reset_a: String::new(),
            reset_b: String::new(),
            tag: clock(base.last_success, self.tz),
            tag_color: theme::NO_DATA.dim,
            extras: Vec::new(),
            source,
        };
        match &base.mode {
            Mode::Live => {
                let p = percent.unwrap_or(0.0);
                feed.ph = phosphor_for(p, self.config);
                feed.tag_color = feed.ph.dim;
                let limit = p >= 100.0;
                if limit {
                    feed.tag = "LIMIT".into();
                    feed.tag_color = feed.ph.fg;
                }
                match resets_at {
                    Some(r) => {
                        let verb = if limit { "RESUMES" } else { "RESETS" };
                        feed.reset_a = format!("{verb} {}", at(r, self.now, self.tz));
                        feed.reset_b = format!("IN {}", left(r, self.now));
                    }
                    None => {
                        // 0% with no reset printed: the window hasn't started.
                        feed.reset_a = "RESETS —".into();
                        feed.reset_b = if p == 0.0 { "NOT STARTED".into() } else { String::new() };
                    }
                }
            }
            Mode::Aux => {
                let p = percent.unwrap_or(0.0);
                feed.ph = phosphor_for(p, self.config);
                feed.tag = "VIA DESKTOP".into();
                feed.tag_color = theme::chrome::AMBER;
                feed.reset_a = "RESETS —".into();
                feed.reset_b = "AUX FEED".into();
            }
            Mode::Acquiring => {
                feed.tag = "--:--:--".into();
                feed.reset_a = "ACQUIRING".into();
                feed.reset_b = "SIGNAL…".into();
            }
            Mode::Stale { .. } => {
                feed.ph = theme::STALE;
                feed.tag_color = theme::STALE.dim;
            }
            Mode::Lost { .. } => {
                feed.ph = theme::LOST;
                feed.tag = "RETRYING".into();
                feed.tag_color = theme::chrome::SUBTLE;
                feed.percent = None;
            }
        }
        feed
    }
}

/// Shared state of one source's feeds.
struct Base {
    mode: Mode,
    last_success: Option<DateTime<Utc>>,
    source: &'static str,
}

fn base(
    source: &'static str,
    has_data: bool,
    aux: bool,
    last_success: Option<DateTime<Utc>>,
    error: Option<&str>,
    interval: u64,
    ctx: &Ctx,
) -> Base {
    let mode = if !has_data {
        match error {
            Some(e) => Mode::Lost { why: e.to_uppercase().chars().take(30).collect() },
            None => Mode::Acquiring,
        }
    } else if is_stale(last_success, Duration::from_secs(interval), ctx.now) {
        Mode::Stale { frozen_at: clock(last_success, ctx.tz) }
    } else if aux {
        Mode::Aux
    } else {
        Mode::Live
    };
    Base { mode, last_success, source }
}

fn claude_section(c: &ClaudeState, ctx: &Ctx) -> Section {
    let logged_out = c.account.as_ref().is_some_and(|a| !a.logged_in);
    let plan = match (&c.account, logged_out) {
        (_, true) => "LOGGED OUT".to_string(),
        (Some(a), false) => a.subscription_type.clone().unwrap_or_else(|| "--".into()).to_uppercase(),
        (None, false) => "--".into(),
    };
    let notice = if logged_out {
        Some("not logged in · run claude auth login".into())
    } else if c.org_mismatch {
        Some("Desktop history is another account · ignored".into())
    } else {
        None
    };
    let has_data = c.source != ClaudeSource::None && (c.limits.session.is_some() || c.limits.week.is_some());
    let b = base(
        "claude",
        has_data,
        c.source == ClaudeSource::Desktop,
        c.last_success,
        c.error.as_deref(),
        ctx.config.usage_interval_seconds,
        ctx,
    );
    let lim = |l: &Option<Limit>| l.as_ref().map(|l| (l.percent as f32, l.resets_at));
    let five = ctx.live_feed("CAM 01", "CLAUDE 5H", lim(&c.limits.session), &b);
    let mut week = ctx.live_feed("CAM 02", "CLAUDE WEEK", lim(&c.limits.week), &b);
    if matches!(b.mode, Mode::Live | Mode::Stale { .. }) {
        week.extras = c
            .limits
            .model_weeks
            .iter()
            .map(|(name, l)| Extra {
                label: format!("{} WK", name.to_uppercase()),
                percent: Some(l.percent as f32),
                ph: if matches!(b.mode, Mode::Stale { .. }) { theme::STALE } else { phosphor_for(l.percent as f32, ctx.config) },
            })
            .collect();
    }
    Section {
        name: "CLAUDE",
        plan,
        plan_color: if logged_out { theme::chrome::AMBER } else { theme::chrome::MUTED },
        email: c.account.as_ref().and_then(|a| a.email.clone()),
        notice,
        feeds: vec![five, week],
    }
}

fn codex_section(x: &CodexState, ctx: &Ctx) -> Section {
    let has_data = x.source != CodexSource::None;
    let b = base("codex", has_data, false, x.last_success, x.error.as_deref(), ctx.config.codex_interval_seconds, ctx);
    let feeds = if x.windows.is_empty() {
        vec![ctx.live_feed("CAM 03", "CODEX", None, &b)]
    } else {
        x.windows
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let cam = if i == 0 { "CAM 03".to_string() } else { format!("CAM 03{}", (b'A' + i as u8) as char) };
                let title = match w.label.as_str() {
                    "5-hour" => "CODEX 5H".to_string(),
                    other => format!("CODEX {}", other.to_uppercase()),
                };
                ctx.live_feed(&cam, &title, Some((w.percent, w.resets_at)), &b)
            })
            .collect()
    };
    let plan = x.account.as_ref().and_then(|a| a.plan_type.clone()).map_or("--".into(), |p| p.to_uppercase());
    Section {
        name: "CODEX",
        plan,
        plan_color: theme::chrome::MUTED,
        email: x.account.as_ref().and_then(|a| a.email.clone()),
        notice: None,
        feeds,
    }
}

pub fn wall(state: &AppState, config: &Config, now: DateTime<Utc>, tz: FixedOffset) -> Wall {
    let ctx = Ctx { config, now, tz };
    let mut sections = Vec::new();
    // A section hides when its CLI is missing (or the user hid it).
    if config.show_claude && state.claude.presence != Presence::Missing {
        sections.push(claude_section(&state.claude, &ctx));
    }
    if config.show_codex && state.codex.presence != Presence::Missing {
        sections.push(codex_section(&state.codex, &ctx));
    }
    let list = &state.sessions.list;
    let status = config.show_status.then(|| {
        let s = &state.claude_status;
        let text = match s.level {
            StatusLevel::Green => "STATUS OK",
            StatusLevel::Yellow => "DEGRADED",
            StatusLevel::Red => "OUTAGE",
            StatusLevel::Unknown => "STATUS --",
        };
        (s.level, text.to_string())
    });
    let footer = Footer {
        status,
        sessions: config.show_sessions.then_some(list.len()),
        busy: list.iter().filter(|s| s.status == SessionStatus::Working).count(),
        waiting: list.iter().filter(|s| matches!(s.status, SessionStatus::Waiting(_))).count(),
    };
    let powered = state.claude.last_success.is_some() || state.codex.last_success.is_some();
    Wall { sections, footer, powered }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn jkt() -> FixedOffset {
        FixedOffset::east_opt(7 * 3600).unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 17, 4, 42, 7).unwrap() // 11:42:07 in Jakarta
    }

    fn live() -> AppState {
        let n = now();
        let mut s = AppState::default();
        s.claude.presence = Presence::Found;
        s.claude.source = ClaudeSource::Cli;
        s.claude.last_success = Some(n);
        s.claude.account = Some(ClaudeAccount {
            logged_in: true,
            email: Some("you@example.com".into()),
            subscription_type: Some("max".into()),
            auth_method: None,
            org_id: None,
        });
        s.claude.limits.session = Some(Limit { percent: 42, resets_at: Some(n + chrono::Duration::minutes(238)) });
        s.claude.limits.week = Some(Limit { percent: 54, resets_at: Some(n + chrono::Duration::minutes(6960)) });
        s.claude.limits.model_weeks = vec![("Fable".into(), Limit { percent: 18, resets_at: None })];
        s.codex.presence = Presence::Found;
        s.codex.source = CodexSource::AppServer;
        s.codex.last_success = Some(n);
        s.codex.windows = vec![CodexWindow { label: "week".into(), bucket: None, window_mins: Some(10080), percent: 38.0, resets_at: None }];
        s
    }

    #[test]
    fn normal_board() {
        let w = wall(&live(), &Config::default(), now(), jkt());
        let c = &w.sections[0];
        assert_eq!((c.name, c.plan.as_str()), ("CLAUDE", "MAX"));
        let f = &c.feeds[0];
        assert_eq!((f.cam.as_str(), f.title.as_str(), f.tag.as_str()), ("CAM 01", "CLAUDE 5H", "11:42:07"));
        assert_eq!((f.reset_a.as_str(), f.reset_b.as_str()), ("RESETS 15:40", "IN 3H 58M"));
        assert_eq!(f.ph, theme::OK);
        let wk = &c.feeds[1];
        assert_eq!(wk.reset_b, "IN 4D 20H");
        assert!(wk.reset_a.starts_with("RESETS ") && wk.reset_a.len() > "RESETS 15:40".len(), "{}", wk.reset_a);
        assert_eq!(wk.extras[0].label, "FABLE WK");
        assert_eq!(wk.screen_height(), 100.0);
        assert_eq!(w.sections[1].feeds[0].title, "CODEX WEEK");
        assert!(w.powered);
    }

    #[test]
    fn levels_and_limit() {
        let mut s = live();
        s.claude.limits.session.as_mut().unwrap().percent = 100;
        s.claude.limits.week.as_mut().unwrap().percent = 71;
        let w = wall(&s, &Config::default(), now(), jkt());
        let f = &w.sections[0].feeds[0];
        assert_eq!((f.tag.as_str(), f.ph), ("LIMIT", theme::CRIT));
        assert!(f.reset_a.starts_with("RESUMES"));
        assert_eq!(w.sections[0].feeds[1].ph, theme::WARN);
    }

    #[test]
    fn stale_desktop_lost_and_boot() {
        let mut s = live();
        s.claude.last_success = Some(now() - chrono::Duration::minutes(31) - chrono::Duration::seconds(32));
        let w = wall(&s, &Config::default(), now(), jkt());
        assert_eq!(w.sections[0].feeds[0].mode, Mode::Stale { frozen_at: "11:10:35".into() });
        assert_eq!(w.sections[0].feeds[0].screen_height(), 92.0);

        let mut s = live();
        s.claude.source = ClaudeSource::Desktop;
        s.claude.account.as_mut().unwrap().logged_in = false;
        s.codex.source = CodexSource::None;
        s.codex.last_success = None;
        s.codex.windows.clear();
        s.codex.error = Some("Codex: unavailable (timeout)".into());
        let w = wall(&s, &Config::default(), now(), jkt());
        let c = &w.sections[0];
        assert_eq!((c.plan.as_str(), c.notice.is_some()), ("LOGGED OUT", true));
        assert_eq!((c.feeds[0].mode.clone(), c.feeds[0].tag.as_str()), (Mode::Aux, "VIA DESKTOP"));
        assert!(c.feeds[1].extras.is_empty());
        assert!(matches!(w.sections[1].feeds[0].mode, Mode::Lost { .. }));

        let w = wall(&AppState::default(), &Config::default(), now(), jkt());
        assert!(!w.powered);
        assert_eq!(w.sections[0].feeds[0].mode, Mode::Acquiring);
        assert_eq!(w.sections[0].feeds[0].reset_a, "ACQUIRING");
    }

    #[test]
    fn missing_cli_hides_section_and_footer_counts() {
        let mut s = live();
        s.codex.presence = Presence::Missing;
        s.sessions.list = vec![
            Session { pid: 1, name: "a".into(), status: SessionStatus::Working, cwd: None, entrypoint: None },
            Session { pid: 2, name: "b".into(), status: SessionStatus::Waiting(None), cwd: None, entrypoint: None },
            Session { pid: 3, name: "c".into(), status: SessionStatus::Idle, cwd: None, entrypoint: None },
        ];
        let w = wall(&s, &Config::default(), now(), jkt());
        assert_eq!(w.sections.len(), 1);
        assert_eq!((w.footer.sessions, w.footer.busy, w.footer.waiting), (Some(3), 1, 1));
    }

    #[test]
    fn zero_without_reset_is_not_started() {
        let mut s = live();
        s.claude.limits.session = Some(Limit { percent: 0, resets_at: None });
        let w = wall(&s, &Config::default(), now(), jkt());
        assert_eq!(w.sections[0].feeds[0].reset_b, "NOT STARTED");
    }
}
