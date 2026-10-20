//! The window, built from the design boards: window chrome (rack, title
//! bar, footer), the compact monitor wall, the details view and Settings.
//!
//! Motion budget (Spec board): REC/WAIT blink at 1 Hz only while a session
//! is busy/waiting, a 400 ms sweep when a feed's source replies, a 3-frame
//! static burst when a feed changes state, countdowns at the minute. Nothing
//! is scheduled while the window is hidden (eframe runs no ui pass then).

pub mod details;
pub mod feeds;
pub mod monitor;
pub mod settings;
pub mod textures;
pub mod theme;

use crate::config::Config;
use crate::model::{AppState, StatusLevel};
use chrono::{DateTime, Utc};
use eframe::egui::{self, Align2, Color32, Rect, Stroke, StrokeKind, pos2, vec2};
use feeds::{Mode, Wall};
use monitor::{Fx, text};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use theme::{chrome, vt};

pub const COMPACT_W: f32 = 320.0;
/// What "100%" (`scale = 1.0`) really draws at. The layout is designed in
/// the units above; this shrinks the whole app so the wide views are 615 px
/// at 100% Windows scaling: 45% of a 1366-wide laptop screen.
pub const BASE_ZOOM: f32 = 615.0 / WIDE_W;

/// egui's zoom for the user's `scale` setting.
pub fn zoom(scale: f32) -> f32 {
    scale * BASE_ZOOM
}

/// Every view other than compact (details, settings) is this wide, and as
/// tall as the taller of them, so switching between them never resizes.
pub const WIDE_W: f32 = 720.0;
/// The settings view's height before it has been drawn (it never changes:
/// its content is fixed), so details matches it from the start.
const SETTINGS_H_GUESS: f32 = 567.5;
const PAD: f32 = 8.0;
const SWEEP: Duration = Duration::from_millis(400);
const BURST_FRAMES: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Compact,
    Details,
    Settings,
}

/// What the window asks the app to do.
pub enum Action {
    Hide,
    StartDrag,
    /// Triple-click on the title bar: forget the dragged position and go
    /// back beside the tray.
    ResetPosition,
    /// Settings applied: the new config and the Start-with-Windows choice.
    Apply(Box<Config>, bool),
    OpenConfig,
}

pub struct WindowUi {
    tex: textures::Textures,
    pub view: View,
    /// Per source: last `last_success` seen and when its sweep started.
    sweeps: HashMap<&'static str, (Option<DateTime<Utc>>, Option<Instant>)>,
    /// Per screen: last mode discriminant and the frame its burst is on.
    modes: HashMap<String, (std::mem::Discriminant<Mode>, Option<u32>)>,
    settings: Option<settings::Draft>,
    blink_epoch: Instant,
    /// Natural (unstretched) heights of the wide views, last time drawn.
    details_h: f32,
    settings_h: f32,
}

impl WindowUi {
    pub fn new(ctx: &egui::Context, config: &Config) -> Self {
        theme::install(ctx);
        let mut ui = Self {
            tex: textures::Textures::new(ctx),
            view: View::Compact,
            sweeps: HashMap::new(),
            modes: HashMap::new(),
            settings: None,
            blink_epoch: Instant::now(),
            details_h: 0.0,
            settings_h: SETTINGS_H_GUESS,
        };
        // Debug builds only: start in another view, for screenshots.
        if cfg!(debug_assertions) {
            match std::env::var("MAJIHANAYA_VIEW").as_deref() {
                Ok("details") => ui.view = View::Details,
                Ok("settings") => ui.open_settings(config),
                _ => {}
            }
        }
        ui
    }

    pub fn open_settings(&mut self, config: &Config) {
        self.settings = Some(settings::Draft::new(config, crate::platform::autostart_enabled()));
        self.view = View::Settings;
    }

    /// Lays out the whole window; returns the content size (for the
    /// viewport) and actions for the app.
    pub fn draw(&mut self, ui: &mut egui::Ui, state: &AppState, config: &Config) -> (egui::Vec2, Vec<Action>) {
        let now = Utc::now();
        let tz = *chrono::Local::now().offset();
        let wall = feeds::wall(state, config, now, tz);
        let view_before = self.view;
        let mut actions = Vec::new();
        let width = if self.view == View::Compact { COMPACT_W } else { WIDE_W };
        // Wide views take on the difference to the taller one.
        let stretch = match self.view {
            View::Compact => 0.0,
            // 0: not drawn yet, so its own height is unknown; no stretch.
            View::Details if self.details_h == 0.0 => 0.0,
            View::Details => (self.settings_h - self.details_h).max(0.0),
            View::Settings => (self.details_h - self.settings_h).max(0.0),
        };

        let ctx = ui.ctx().clone();
        let blink_on = (self.blink_epoch.elapsed().as_secs() % 2) == 0;
        let animating = self.track_sweeps(state, &wall);

        let frame = egui::Frame::new()
            .fill(chrome::RACK)
            .stroke(Stroke::new(1.0, chrome::BORDER))
            .corner_radius(10)
            .inner_margin(PAD);
        // The content sizes the window, not the other way round: lay it out in
        // an area as large as it may need, or a growing view (details,
        // settings) is measured and clipped at the current window size and
        // the window never grows. Drawing outside the window is just clipped
        // until the resize lands a frame later.
        let area = Rect::from_min_size(ui.max_rect().min, vec2(width, 4096.0));
        let mut outer = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let resp = frame.show(&mut outer, |ui| {
            ui.set_width(width - 2.0 * PAD - 2.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            self.title_bar(ui, &wall, config, &mut actions);
            match self.view {
                View::Compact => self.compact(ui, &wall, blink_on),
                View::Details => details::draw(ui, &self.tex, &wall, state, config, now, tz, blink_on, &|f| self_fx(&self.sweeps, &self.modes, f), stretch),
                View::Settings => {
                    if let Some(draft) = &mut self.settings {
                        match settings::draw(ui, draft, stretch) {
                            Some(settings::Outcome::Apply(cfg, autostart)) => {
                                actions.push(Action::Apply(cfg, autostart));
                                self.view = View::Compact;
                            }
                            Some(settings::Outcome::Cancel) => self.view = View::Compact,
                            Some(settings::Outcome::OpenConfig) => actions.push(Action::OpenConfig),
                            None => {}
                        }
                    }
                }
            }
            if self.view != View::Settings {
                self.footer(ui, &wall, state, blink_on);
            }
        });
        let size = resp.response.rect.size();
        // Remember this view's own height (what it is without the stretch),
        // unless a click switched views mid-frame and this was the old one.
        if self.view == view_before {
            match self.view {
                View::Details => self.details_h = size.y - stretch,
                View::Settings => self.settings_h = size.y - stretch,
                View::Compact => {}
            }
        }

        // A click switched views mid-frame: this frame was laid out for the
        // old one, so draw the new one right away.
        if self.view != view_before {
            ctx.request_repaint();
        }
        // Schedule only what's moving: sweeps/bursts at ~30 fps, blink at 1 Hz,
        // otherwise the next minute (countdowns).
        let blinking = wall.footer.busy + wall.footer.waiting > 0;
        if animating {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if blinking {
            let ms = 1000 - (self.blink_epoch.elapsed().as_millis() % 1000) as u64;
            ctx.request_repaint_after(Duration::from_millis(ms.max(1)));
        }
        (size, actions)
    }

    /// Starts a sweep when a source's `last_success` moves, a burst when a
    /// screen changes state; ages both. Returns whether anything animates.
    fn track_sweeps(&mut self, state: &AppState, wall: &Wall) -> bool {
        let mut animating = false;
        for (source, last) in [("claude", state.claude.last_success), ("codex", state.codex.last_success)] {
            let entry = self.sweeps.entry(source).or_insert((last, None));
            if entry.0 != last {
                *entry = (last, Some(Instant::now()));
            }
            if entry.1.is_some_and(|t| t.elapsed() >= SWEEP) {
                entry.1 = None;
            }
            animating |= entry.1.is_some();
        }
        for f in wall.sections.iter().flat_map(|s| &s.feeds) {
            let d = std::mem::discriminant(&f.mode);
            let key = format!("{}{}", f.cam, f.title);
            let entry = self.modes.entry(key).or_insert((d, None));
            if entry.0 != d {
                *entry = (d, Some(0));
            } else if let Some(n) = entry.1 {
                entry.1 = (n + 1 < BURST_FRAMES).then_some(n + 1);
            }
            animating |= entry.1.is_some();
        }
        animating
    }

    fn title_bar(&mut self, ui: &mut egui::Ui, wall: &Wall, config: &Config, actions: &mut Vec<Action>) {
        let (bar, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), egui::Sense::click_and_drag());
        if resp.drag_started() {
            actions.push(Action::StartDrag);
        }
        if resp.triple_clicked() {
            actions.push(Action::ResetPosition);
        }
        let p = ui.painter();
        let led = if wall.powered { chrome::GREEN } else { theme::NO_DATA.fg };
        let c = pos2(bar.left() + 7.5, bar.center().y);
        if wall.powered {
            p.circle_filled(c, 5.0, led.gamma_multiply(0.25));
        }
        p.circle_filled(c, 3.5, led);
        let tr = text(p, pos2(bar.left() + 16.0, bar.center().y), Align2::LEFT_CENTER, "MAJIHANAYA", vt(19.0), chrome::PLATE_TEXT, 3.0);
        let sub = match self.view {
            View::Compact => None,
            View::Details => Some("· MONITOR WALL"),
            View::Settings => Some("· SETTINGS"),
        };
        if let Some(s) = sub {
            text(p, pos2(tr.right() + 8.0, bar.center().y), Align2::LEFT_CENTER, s, vt(17.0), chrome::MUTED, 2.0);
        }

        // Buttons from the right: hide, details toggle, settings (gear).
        let mut x = bar.right() - 2.0;
        let mut button = |ui: &mut egui::Ui, icon: Icon, pressed: bool, label: &str| -> bool {
            let r = Rect::from_min_size(pos2(x - 24.0, bar.top()), vec2(24.0, 24.0));
            x -= 28.0;
            let resp = ui.interact(r, ui.scope_id().with(label), egui::Sense::click()).on_hover_text(label);
            let p = ui.painter();
            let (bg, border, fg) = if pressed {
                (chrome::BORDER, chrome::BORDER_HI, chrome::GREEN)
            } else {
                (chrome::BEZEL, if resp.hovered() { chrome::BORDER_HI } else { chrome::BORDER }, chrome::PLATE_TEXT)
            };
            p.rect_filled(r, 5.0, bg);
            p.rect_stroke(r, 5.0, Stroke::new(1.0, border), StrokeKind::Inside);
            if resp.has_focus() {
                p.rect_stroke(r.expand(2.0), 6.0, Stroke::new(1.0, chrome::GREEN), StrokeKind::Outside);
            }
            icon.paint(p, r.center(), fg);
            resp.clicked()
        };
        if self.view == View::Settings {
            if button(ui, Icon::Close, false, "Close settings without saving") {
                self.view = View::Compact;
            }
            return;
        }
        if button(ui, Icon::Hide, false, "Hide window") {
            actions.push(Action::Hide);
        }
        let details = self.view == View::Details;
        if button(ui, if details { Icon::Collapse } else { Icon::Expand }, details, if details { "Hide details" } else { "Show details" }) {
            self.view = if details { View::Compact } else { View::Details };
        }
        if button(ui, Icon::Gear, false, "Settings") {
            self.open_settings(config);
        }
    }

    fn compact(&mut self, ui: &mut egui::Ui, wall: &Wall, _blink_on: bool) {
        let w = ui.available_width();
        for sec in &wall.sections {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                monitor::plate(ui, w, sec.name, &sec.plan, sec.plan_color, None);
                if let Some(n) = &sec.notice {
                    monitor::notice(ui, w, n);
                }
                for f in &sec.feeds {
                    monitor::feed_monitor(ui, &self.tex, w, f, self_fx(&self.sweeps, &self.modes, f));
                }
            });
        }
        if wall.sections.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(w, 60.0), egui::Sense::hover());
            text(ui.painter(), r.center(), Align2::CENTER_CENTER, "NO FEEDS · INSTALL claude OR codex", vt(17.0), chrome::MUTED, 1.0);
        }
    }

    fn footer(&self, ui: &mut egui::Ui, wall: &Wall, state: &AppState, blink_on: bool) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), egui::Sense::hover());
        let p = ui.painter();
        let mid = r.center().y;
        let mut x = r.left() + 4.0;
        if self.view == View::Details {
            let v = &state.version;
            if state.claude.desktop_only {
                // The version found is Claude Desktop's bundled copy, which
                // the user didn't install and Desktop updates itself: showing
                // it (or "update available") would contradict the notice.
                text(p, pos2(x, mid), Align2::LEFT_CENTER, "CLAUDE CODE NOT INSTALLED", vt(17.0), chrome::AMBER, 1.0);
            } else if let Some(local) = &v.local {
                let lr = text(p, pos2(x, mid), Align2::LEFT_CENTER, &format!("CLAUDE CODE {local}"), vt(17.0), chrome::PLATE_TEXT, 1.0);
                x = lr.right() + 8.0;
                if let Some(latest) = v.latest.as_ref().filter(|l| *l != local) {
                    text(p, pos2(x, mid), Align2::LEFT_CENTER, &format!("↑ {latest} LATEST"), vt(17.0), chrome::AMBER, 1.0);
                }
            }
        } else if let Some((level, label)) = &wall.footer.status {
            let led = match level {
                StatusLevel::Green => chrome::GREEN,
                StatusLevel::Yellow => chrome::AMBER,
                StatusLevel::Red => theme::CRIT.fg,
                StatusLevel::Unknown => theme::NO_DATA.fg,
            };
            p.circle_filled(pos2(x + 3.5, mid), 3.5, led);
            text(p, pos2(x + 13.0, mid), Align2::LEFT_CENTER, label, vt(17.0), chrome::PLATE_TEXT, 1.0);
        }

        // Chips from the right: WAIT n, REC n, SESS n.
        let mut rx = r.right() - 4.0;
        let mut chip = |s: &str, fg: Color32, bg: Option<Color32>| {
            let probe = text(p, pos2(rx, mid), Align2::RIGHT_CENTER, s, vt(17.0), Color32::TRANSPARENT, 1.0);
            if let Some(bg) = bg {
                p.rect_filled(probe.expand2(vec2(5.0, 0.0)), 3.0, bg);
            }
            text(p, pos2(rx, mid), Align2::RIGHT_CENTER, s, vt(17.0), fg, 1.0);
            rx = probe.left() - 13.0;
        };
        let f = &wall.footer;
        if f.waiting > 0 {
            let (fg, bg) = if blink_on { (chrome::RACK, chrome::AMBER) } else { (chrome::AMBER, chrome::NOTICE_BG) };
            chip(&format!("◆ WAIT {}", f.waiting), fg, Some(bg));
        }
        if f.busy > 0 {
            chip(&format!("● REC {}", f.busy), if blink_on { chrome::REC_ON } else { chrome::REC_OFF }, None);
        }
        if let Some(n) = f.sessions {
            chip(&format!("SESS {n}"), chrome::PLATE_TEXT, None);
        }
    }
}

/// The sweep/burst state for one feed screen this frame.
pub fn self_fx(
    sweeps: &HashMap<&'static str, (Option<DateTime<Utc>>, Option<Instant>)>,
    modes: &HashMap<String, (std::mem::Discriminant<Mode>, Option<u32>)>,
    f: &feeds::Feed,
) -> Fx {
    let sweep = sweeps
        .get(f.source)
        .and_then(|(_, t)| *t)
        .map(|t| (t.elapsed().as_secs_f32() / SWEEP.as_secs_f32()).min(1.0));
    let burst = modes.get(&format!("{}{}", f.cam, f.title)).and_then(|(_, b)| *b);
    Fx { sweep, burst }
}

#[derive(Clone, Copy)]
enum Icon {
    Expand,
    Collapse,
    Hide,
    Gear,
    Close,
}

impl Icon {
    /// 14 px stroke icons from the boards, drawn as lines.
    fn paint(self, p: &egui::Painter, c: egui::Pos2, color: Color32) {
        let s = Stroke::new(1.4, color);
        let o = c - vec2(7.0, 7.0);
        let pt = |x: f32, y: f32| o + vec2(x, y);
        match self {
            Icon::Expand | Icon::Collapse => {
                p.rect_stroke(Rect::from_min_max(pt(1.5, 2.5), pt(12.5, 11.5)), 1.5, s, StrokeKind::Middle);
                let pts = if matches!(self, Icon::Expand) { [pt(6.0, 5.0), pt(8.0, 7.0), pt(6.0, 9.0)] } else { [pt(8.0, 5.0), pt(6.0, 7.0), pt(8.0, 9.0)] };
                p.line(pts.to_vec(), s);
            }
            Icon::Hide => {
                p.line_segment([pt(3.0, 7.0), pt(11.0, 7.0)], s);
            }
            Icon::Close => {
                p.line_segment([pt(3.5, 3.5), pt(10.5, 10.5)], s);
                p.line_segment([pt(10.5, 3.5), pt(3.5, 10.5)], s);
            }
            Icon::Gear => {
                p.circle_stroke(c, 2.2, s);
                for i in 0..8 {
                    let a = i as f32 * std::f32::consts::FRAC_PI_4;
                    let d = vec2(a.cos(), a.sin());
                    p.line_segment([c + d * 4.0, c + d * 6.0], s);
                }
            }
        }
    }
}
