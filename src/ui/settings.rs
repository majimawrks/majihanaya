//! Settings panel (632 wide): see the design's Settings board.
//! Controls are painted by hand (switch, radio, stepper, slider) so they
//! match the board; each one is a focusable `Response` (Tab, Space/Enter,
//! arrow keys). No animation: repaint on input only.

use super::monitor::text;
use super::theme::{chrome, hex, mono, vt};
use crate::config::{Config, TrayNumber};
use eframe::egui::{
    self, Align2, Color32, EventFilter, Key, Margin, Modifiers, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

/// The edited copy of the settings, compared with what was loaded.
pub struct Draft {
    pub saved: Config,
    pub cur: Config,
    pub autostart_saved: bool,
    pub autostart: bool,
}

pub enum Outcome {
    Apply(Box<Config>, bool),
    Cancel,
    OpenConfig,
}

const CHANGED: Color32 = chrome::AMBER;
const RED: Color32 = hex(0xF87171);
const FIELD_BG: Color32 = hex(0x06120A);

fn feq(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.001
}

/// Warn is 1..=99; raising it past crit carries crit with it.
pub fn set_warn(c: &mut Config, v: i32) {
    let w = v.clamp(1, 99) as u8;
    c.warn_percent = w;
    c.crit_percent = c.crit_percent.max(w);
}

/// Crit is warn..=100.
pub fn set_crit(c: &mut Config, v: i32) {
    c.crit_percent = v.clamp(c.warn_percent as i32, 100) as u8;
}

/// Snap to the slider's step grid, rounded to 2 decimals.
pub fn quantize(v: f32, min: f32, max: f32, step: f32) -> f32 {
    let q = min + ((v.clamp(min, max) - min) / step).round() * step;
    ((q * 100.0).round() / 100.0).clamp(min, max)
}

impl Draft {
    pub fn new(config: &Config, autostart: bool) -> Self {
        Self { saved: config.clone(), cur: config.clone(), autostart_saved: autostart, autostart }
    }

    /// Fields (autostart included) that differ from what was loaded.
    pub fn changes(&self) -> usize {
        let (c, s) = (&self.cur, &self.saved);
        [
            c.tray_number != s.tray_number,
            c.tray_reset_clock != s.tray_reset_clock,
            c.warn_percent != s.warn_percent,
            c.crit_percent != s.crit_percent,
            c.show_claude != s.show_claude,
            c.show_codex != s.show_codex,
            c.show_sessions != s.show_sessions,
            c.show_status != s.show_status,
            c.show_account != s.show_account,
            c.show_stats != s.show_stats,
            c.use_desktop_fallback != s.use_desktop_fallback,
            !feq(c.scale, s.scale),
            !feq(c.opacity, s.opacity),
            c.topmost != s.topmost,
            self.autostart != self.autostart_saved,
        ]
        .iter()
        .filter(|b| **b)
        .count()
    }

    /// Reset to defaults changes `cur` only; window position and unknown keys stay.
    pub fn reset_defaults(&mut self) {
        let (left, top, extra) = (self.cur.left, self.cur.top, std::mem::take(&mut self.cur.extra));
        self.cur = Config { left, top, extra, ..Config::default() };
    }
}

// ---- small drawing helpers ----

fn ctl(ui: &Ui, rect: Rect, name: &str, sense: Sense) -> Response {
    let r = ui.interact(rect, ui.scope_id().with(name), sense);
    if sense != Sense::hover() { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

fn ring(p: &Painter, rect: Rect, radius: f32, r: &Response) {
    if r.has_focus() {
        p.rect_stroke(rect.expand(1.0), radius, Stroke::new(1.0, chrome::GREEN), StrokeKind::Outside);
    }
}

/// Arrow keys pressed while `r` has focus: (horizontal, vertical), down = +1.
fn arrows(ui: &Ui, r: &Response) -> (i32, i32) {
    if !r.has_focus() {
        return (0, 0);
    }
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(r.id, EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() })
    });
    ui.input_mut(|i| {
        let k = |i: &mut egui::InputState, key| i.consume_key(Modifiers::NONE, key) as i32;
        let h = k(i, Key::ArrowRight) - k(i, Key::ArrowLeft);
        let v = k(i, Key::ArrowDown) - k(i, Key::ArrowUp);
        (h, v)
    })
}

/// Changed-value dot ending at `right`; returns the x where the next item ends.
fn mark(p: &Painter, right: f32, cy: f32, on: bool) -> f32 {
    if on {
        p.circle_filled(pos2(right - 3.0, cy), 3.0, CHANGED);
    }
    right - 6.0 - 8.0
}

fn label(p: &Painter, pos: Pos2, align: Align2, s: &str, size: f32, color: Color32) -> Rect {
    text(p, pos, align, s, mono(size), color, 0.0)
}

/// Width of `s` in VT323 `px` with letter spacing 1 (nothing is painted).
fn vt_width(ui: &Ui, s: &str, px: f32) -> f32 {
    text(&ui.painter().with_clip_rect(Rect::NOTHING), Pos2::ZERO, Align2::LEFT_TOP, s, vt(px), chrome::INK, 1.0).width()
}

/// A titled group: plate, then a bordered body with `gap` between rows.
fn group(ui: &mut Ui, title: &str, sub: &str, gap: f32, body: impl FnOnce(&mut Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        let (plate, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(plate, 3.0, chrome::PLATE);
        text(p, plate.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, title, vt(17.0), chrome::PLATE_TEXT, 2.0);
        text(p, plate.right_center() - vec2(6.0, 0.0), Align2::RIGHT_CENTER, sub, vt(17.0), chrome::MUTED, 2.0);
        egui::Frame::new()
            .fill(chrome::GROUP_BG)
            .stroke(Stroke::new(1.0, chrome::BORDER))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = gap;
                let w = ui.available_width();
                ui.set_width(w);
                body(ui);
            });
    });
}

/// Switch row (26 high); the whole row toggles.
fn switch(ui: &mut Ui, name: &str, text_: &str, hint: &str, value: &mut bool, saved: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let r = ctl(ui, rect, name, Sense::click());
    if r.clicked() {
        *value = !*value;
    }
    let on = *value;
    let p = ui.painter();
    let cy = rect.center().y;
    let track = Rect::from_center_size(pos2(rect.right() - 16.0, cy), vec2(32.0, 16.0));
    let (tf, tb, kc, kx) = if on {
        (chrome::SELECTED_BG, chrome::GREEN_DIM, chrome::GREEN, 18.0)
    } else {
        (chrome::PLATE, chrome::BORDER_HI, chrome::MUTED, 2.0)
    };
    p.rect_filled(track, 8.0, tf);
    p.rect_stroke(track, 8.0, Stroke::new(1.0, tb), StrokeKind::Inside);
    p.circle_filled(pos2(track.left() + kx + 5.0, cy), 5.0, kc);
    let x = mark(p, track.left() - 8.0, cy, on != saved);
    if !hint.is_empty() {
        label(p, pos2(x, cy), Align2::RIGHT_CENTER, hint, 10.0, chrome::MUTED);
    }
    label(p, pos2(rect.left(), cy), Align2::LEFT_CENTER, text_, 11.0, chrome::INK);
    ring(p, rect.expand2(vec2(2.0, 0.0)), 4.0, &r);
}

/// Returns the requested change (-1, 0, +1 per event).
fn stepper(ui: &mut Ui, name: &str, text_: &str, swatch: Color32, value: u8, changed: bool) -> i32 {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let cy = rect.center().y;
    let plus = Rect::from_center_size(pos2(rect.right() - 12.0 - 8.0 - 12.0, cy), vec2(24.0, 24.0));
    let field = Rect::from_center_size(pos2(plus.left() - 8.0 - 22.0, cy), vec2(44.0, 24.0));
    let minus = Rect::from_center_size(pos2(field.left() - 8.0 - 12.0, cy), vec2(24.0, 24.0));
    let rm = ctl(ui, minus, &format!("{name}-dn"), Sense::click());
    let rf = ctl(ui, field, &format!("{name}-in"), Sense::click());
    let rp = ctl(ui, plus, &format!("{name}-up"), Sense::click());
    let (h, v) = arrows(ui, &rf);
    let mut delta = h - v;
    if rm.clicked() {
        delta -= 1;
    }
    if rp.clicked() {
        delta += 1;
    }
    let p = ui.painter();
    p.rect_filled(Rect::from_center_size(pos2(rect.left() + 4.0, cy), vec2(8.0, 8.0)), 2.0, swatch);
    label(p, pos2(rect.left() + 16.0, cy), Align2::LEFT_CENTER, text_, 11.0, chrome::INK);
    mark(p, minus.left() - 8.0 + 8.0, cy, changed);
    for (b, s, r) in [(minus, "\u{2212}", &rm), (plus, "+", &rp)] {
        p.rect_filled(b, 5.0, chrome::BEZEL);
        p.rect_stroke(b, 5.0, Stroke::new(1.0, chrome::BORDER), StrokeKind::Inside);
        text(p, b.center(), Align2::CENTER_CENTER, s, vt(18.0), chrome::PLATE_TEXT, 0.0);
        ring(p, b, 5.0, r);
    }
    p.rect_filled(field, 4.0, FIELD_BG);
    p.rect_stroke(field, 4.0, Stroke::new(1.0, chrome::BORDER), StrokeKind::Inside);
    text(p, field.right_center() - vec2(5.0, 0.0), Align2::RIGHT_CENTER, &value.to_string(), vt(20.0), swatch, 0.0);
    ring(p, field, 4.0, &rf);
    text(p, pos2(rect.right(), cy), Align2::RIGHT_CENTER, "%", vt(17.0), chrome::MUTED, 0.0);
    delta
}

fn slider(ui: &mut Ui, name: &str, text_: &str, value: &mut f32, (min, max, step): (f32, f32, f32), shown: &str, changed: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let cy = rect.center().y;
    let rail_l = rect.left() + 64.0 + 8.0;
    let rail_r = rect.right() - 48.0 - 8.0 - 6.0 - 8.0;
    let hit = Rect::from_min_max(pos2(rail_l, rect.top()), pos2(rail_r, rect.bottom()));
    let r = ctl(ui, hit, name, Sense::click_and_drag());
    let (x0, x1) = (rail_l + 6.0, rail_r - 6.0);
    if let (true, Some(pt)) = (r.dragged() || r.clicked(), r.interact_pointer_pos()) {
        let t = ((pt.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
        *value = quantize(min + t * (max - min), min, max, step);
    }
    let (h, v) = arrows(ui, &r);
    if h - v != 0 {
        *value = quantize(*value + (h - v) as f32 * step, min, max, step);
    }
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let tx = x0 + t * (x1 - x0);
    let p = ui.painter();
    label(p, pos2(rect.left(), cy), Align2::LEFT_CENTER, text_, 11.0, chrome::INK);
    let rail = Rect::from_min_max(pos2(x0, cy - 2.0), pos2(x1, cy + 2.0));
    p.rect_filled(rail, 2.0, chrome::PLATE);
    p.rect_filled(Rect::from_min_max(rail.min, pos2(tx, rail.max.y)), 2.0, chrome::GREEN);
    p.circle_filled(pos2(tx, cy), 6.0, chrome::GREEN);
    ring(p, hit, 4.0, &r);
    mark(p, rect.right() - 48.0, cy, changed);
    text(p, pos2(rect.right(), cy), Align2::RIGHT_CENTER, shown, vt(19.0), chrome::GREEN, 0.0);
}

/// Radio group (rows 26 high); arrows move the selection.
fn radios(ui: &mut Ui, opts: &[(&str, &str)], sel: usize, saved: usize) -> usize {
    let mut hits = Vec::new();
    let mut resp = Vec::new();
    for i in 0..opts.len() {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        let hit = rect.expand2(vec2(6.0, 0.0));
        resp.push(ctl(ui, hit, &format!("radio{i}"), Sense::click()));
        hits.push(hit);
    }
    let mut new = sel;
    for (i, r) in resp.iter().enumerate() {
        if r.clicked() {
            new = i;
        }
        let (h, v) = arrows(ui, r);
        if h + v != 0 {
            new = (i as i32 + h + v).clamp(0, opts.len() as i32 - 1) as usize;
            ui.memory_mut(|m| m.request_focus(resp[new].id));
        }
    }
    let p = ui.painter();
    for (i, ((name, hint), r)) in opts.iter().zip(&resp).enumerate() {
        let hit = hits[i];
        let on = i == new;
        let cy = hit.center().y;
        if on {
            p.rect_filled(hit, 4.0, chrome::SELECTED_BG);
        }
        let c = pos2(hit.left() + 6.0 + 7.0, cy);
        p.circle_stroke(c, 6.5, Stroke::new(1.0, if on { chrome::GREEN } else { chrome::BORDER_HI }));
        if on {
            p.circle_filled(c, 3.5, chrome::GREEN);
        }
        let x = mark(p, hit.right() - 6.0, cy, on && new != saved);
        if !hint.is_empty() {
            label(p, pos2(x, cy), Align2::RIGHT_CENTER, hint, 10.0, chrome::MUTED);
        }
        label(p, pos2(c.x + 7.0 + 8.0, cy), Align2::LEFT_CENTER, name, 11.0, if on { chrome::GREEN } else { chrome::INK });
        ring(p, hit, 4.0, r);
    }
    new
}

struct Btn<'a> {
    name: &'a str,
    s: &'a str,
    px: f32,
    pad: f32,
    fg: Color32,
    fill: Option<Color32>,
    border: Color32,
    enabled: bool,
    underline: bool,
}

/// Footer button anchored at `x` (its right edge, or left edge when `from_left`).
/// Returns the response and the opposite edge x.
fn button(ui: &mut Ui, b: Btn, x: f32, y: f32, from_left: bool) -> (Response, f32) {
    let w = vt_width(ui, b.s, b.px) + 2.0 * b.pad;
    let left = if from_left { x } else { x - w };
    let rect = Rect::from_min_size(pos2(left, y), vec2(w, 28.0));
    let r = ctl(ui, rect, b.name, if b.enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if let Some(f) = b.fill {
        p.rect_filled(rect, 5.0, f);
    }
    p.rect_stroke(rect, 5.0, Stroke::new(1.0, b.border), StrokeKind::Inside);
    let tr = text(p, rect.center(), Align2::CENTER_CENTER, b.s, vt(b.px), b.fg, 1.0);
    if b.underline {
        p.hline(tr.x_range(), tr.bottom() - 2.0, Stroke::new(1.0, b.fg));
    }
    ring(p, rect, 5.0, &r);
    (r, if from_left { rect.right() } else { rect.left() })
}

pub fn draw(ui: &mut Ui, draft: &mut Draft) -> Option<Outcome> {
    let mut out = None;
    let saved = draft.saved.clone();
    let autostart_saved = draft.autostart_saved;

    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
    ui.columns(2, |cols| {
        for c in cols.iter_mut() {
            c.spacing_mut().item_spacing = vec2(8.0, 8.0);
        }
        let cfg = &mut draft.cur;
        let autostart = &mut draft.autostart;

        // ---- left ----
        group(&mut cols[0], "TRAY", "ICON NUMBER", 0.0, |ui| {
            const T: [TrayNumber; 4] =
                [TrayNumber::Worst, TrayNumber::ClaudeSession, TrayNumber::ClaudeWeek, TrayNumber::Codex];
            let opts = [("Worst window", "default"), ("Claude 5-hour only", ""), ("Claude week only", ""), ("Codex worst window", "")];
            let idx = |t: TrayNumber| T.iter().position(|x| *x == t).unwrap_or(0);
            let n = radios(ui, &opts, idx(cfg.tray_number), idx(saved.tray_number));
            cfg.tray_number = T[n];
            ui.add_space(4.0);
            let (d, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(d, 0.0, chrome::BORDER);
            ui.add_space(4.0);
            switch(ui, "reset-clock", "Show reset clock in tray tooltip", "", &mut cfg.tray_reset_clock, saved.tray_reset_clock);
        });
        group(&mut cols[0], "THRESHOLDS", "PHOSPHOR LEVELS", 4.0, |ui| {
            let d = stepper(ui, "warn", "Warn at", CHANGED, cfg.warn_percent, cfg.warn_percent != saved.warn_percent);
            if d != 0 {
                set_warn(cfg, cfg.warn_percent as i32 + d);
            }
            let d = stepper(ui, "crit", "Crit at", RED, cfg.crit_percent, cfg.crit_percent != saved.crit_percent);
            if d != 0 {
                set_crit(cfg, cfg.crit_percent as i32 + d);
            }
            let note = "Crit can\u{2019}t go below warn: raising warn past crit carries crit with it.";
            let g = ui.painter().layout(note.into(), mono(10.0), chrome::SUBTLE, ui.available_width());
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y), Sense::hover());
            ui.painter().galley(r.min, g, chrome::SUBTLE);
        });
        group(&mut cols[0], "WINDOW", "LOOK", 2.0, |ui| {
            let ch = !feq(cfg.scale, saved.scale);
            let shown = format!("{:.2}\u{00D7}", cfg.scale);
            slider(ui, "scale", "Scale", &mut cfg.scale, (0.75, 2.0, 0.05), &shown, ch);
            let ch = !feq(cfg.opacity, saved.opacity);
            let shown = format!("{}%", (cfg.opacity * 100.0).round() as i32);
            slider(ui, "opacity", "Opacity", &mut cfg.opacity, (0.5, 1.0, 0.05), &shown, ch);
            switch(ui, "topmost", "Always on top", "", &mut cfg.topmost, saved.topmost);
        });

        // ---- right ----
        group(&mut cols[1], "FEEDS", "WHAT THE WALL SHOWS", 0.0, |ui| {
            switch(ui, "f-claude", "Show Claude", "", &mut cfg.show_claude, saved.show_claude);
            switch(ui, "f-codex", "Show Codex", "", &mut cfg.show_codex, saved.show_codex);
            switch(ui, "f-sessions", "Show sessions", "", &mut cfg.show_sessions, saved.show_sessions);
            switch(ui, "f-status", "Show service status", "", &mut cfg.show_status, saved.show_status);
            switch(ui, "f-account", "Show account", "", &mut cfg.show_account, saved.show_account);
            switch(ui, "f-stats", "Show 24h / 7d stats", "", &mut cfg.show_stats, saved.show_stats);
            switch(ui, "f-fallback", "Claude Desktop fallback", "history file", &mut cfg.use_desktop_fallback, saved.use_desktop_fallback);
        });
        group(&mut cols[1], "SYSTEM", "THIS PC", 4.0, |ui| {
            switch(ui, "autostart", "Start with Windows", "HKCU Run key", autostart, autostart_saved);

            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
            let cy = rect.center().y;
            let bw = vt_width(ui, "OPEN CONFIG FILE", 17.0) + 20.0;
            let b = Rect::from_center_size(pos2(rect.right() - bw / 2.0, cy), vec2(bw, 24.0));
            let r = ctl(ui, b, "open-config", Sense::click());
            if r.clicked() {
                out = Some(Outcome::OpenConfig);
            }
            let p = ui.painter();
            let clip = Rect::from_min_max(rect.min, pos2(b.left() - 8.0, rect.max.y));
            label(&p.with_clip_rect(clip), pos2(rect.left(), cy), Align2::LEFT_CENTER, "%APPDATA%\\majihanaya\\config.json", 10.0, chrome::SUBTLE);
            p.rect_filled(b, 5.0, chrome::BEZEL);
            p.rect_stroke(b, 5.0, Stroke::new(1.0, chrome::BORDER_HI), StrokeKind::Inside);
            text(p, b.center(), Align2::CENTER_CENTER, "OPEN CONFIG FILE", vt(17.0), chrome::INK, 1.0);
            ring(p, b, 5.0, &r);

            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
            let cy = rect.center().y;
            let p = ui.painter();
            let lr = label(p, pos2(rect.left(), cy), Align2::LEFT_CENTER, "Language ", 11.0, chrome::MUTED);
            label(p, pos2(lr.right(), cy), Align2::LEFT_CENTER, "\u{00B7} more later", 10.0, chrome::MUTED);
            let sel = Rect::from_center_size(pos2(rect.right() - 36.0, cy), vec2(72.0, 24.0));
            p.rect_filled(sel, 4.0, chrome::GROUP_BG);
            p.rect_stroke(sel, 4.0, Stroke::new(1.0, chrome::BORDER), StrokeKind::Inside);
            text(p, sel.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, "EN", vt(17.0), chrome::MUTED, 0.0);
        });
    });

    // ---- footer ----
    let n = draft.changes();
    let (foot, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
    ui.painter().hline(foot.x_range(), foot.top() + 0.5, Stroke::new(1.0, chrome::BORDER));
    let y = foot.top() + 8.0;
    let apply_btn = if n == 0 {
        Btn { name: "apply", s: "APPLY", px: 18.0, pad: 16.0, fg: chrome::MUTED, fill: Some(chrome::BEZEL), border: chrome::BORDER, enabled: false, underline: false }
    } else {
        Btn { name: "apply", s: "APPLY", px: 18.0, pad: 16.0, fg: chrome::RACK, fill: Some(chrome::GREEN), border: chrome::GREEN, enabled: true, underline: false }
    };
    let (apply, x) = button(ui, apply_btn, foot.right() - 2.0, y, false);
    let cancel_btn = Btn { name: "cancel", s: "CANCEL", px: 18.0, pad: 14.0, fg: chrome::INK, fill: Some(chrome::BEZEL), border: chrome::BORDER_HI, enabled: true, underline: false };
    let (cancel, _) = button(ui, cancel_btn, x - 8.0, y, false);
    let reset_btn = Btn { name: "reset", s: "RESET TO DEFAULTS", px: 17.0, pad: 10.0, fg: chrome::PLATE_TEXT, fill: None, border: Color32::TRANSPARENT, enabled: true, underline: true };
    let (reset, _) = button(ui, reset_btn, foot.left() + 2.0, y, true);
    let (msg, col) = if n == 0 {
        ("NO CHANGES".to_string(), chrome::MUTED)
    } else {
        (format!("{n} {} \u{00B7} UNSAVED", if n == 1 { "CHANGE" } else { "CHANGES" }), CHANGED)
    };
    text(ui.painter(), pos2(foot.center().x, y + 14.0), Align2::CENTER_CENTER, &msg, vt(17.0), col, 1.0);

    if reset.clicked() {
        draft.reset_defaults();
    }
    if apply.clicked() && n > 0 {
        out = Some(Outcome::Apply(Box::new(draft.cur.clone()), draft.autostart));
    }
    if cancel.clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
        out = Some(Outcome::Cancel);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> Draft {
        Draft::new(&Config::default(), false)
    }

    #[test]
    fn warn_carries_crit() {
        let mut c = Config::default();
        set_warn(&mut c, 90);
        assert_eq!((c.warn_percent, c.crit_percent), (90, 90));
        set_warn(&mut c, 200);
        assert_eq!((c.warn_percent, c.crit_percent), (99, 99));
        set_warn(&mut c, -4);
        assert_eq!(c.warn_percent, 1);
        assert_eq!(c.crit_percent, 99);
    }

    #[test]
    fn crit_bounded_by_warn() {
        let mut c = Config::default();
        set_crit(&mut c, 10);
        assert_eq!(c.crit_percent, c.warn_percent);
        set_crit(&mut c, 500);
        assert_eq!(c.crit_percent, 100);
    }

    #[test]
    fn quantize_steps() {
        assert!(feq(quantize(1.0123, 0.75, 2.0, 0.05), 1.0));
        assert!(feq(quantize(9.0, 0.75, 2.0, 0.05), 2.0));
        assert!(feq(quantize(0.0, 0.5, 1.0, 0.05), 0.5));
        assert!(feq(quantize(0.92, 0.5, 1.0, 0.05), 0.9));
    }

    #[test]
    fn counts_changes_including_autostart() {
        let mut d = draft();
        assert_eq!(d.changes(), 0);
        d.cur.show_stats = false;
        d.cur.scale = 1.3;
        d.autostart = true;
        assert_eq!(d.changes(), 3);
        d.cur.scale = 1.0;
        assert_eq!(d.changes(), 2);
        d.cur.left = Some(5.0);
        assert_eq!(d.changes(), 2, "window position is not a setting");
    }

    #[test]
    fn reset_keeps_position_and_extra() {
        let mut d = draft();
        d.cur.left = Some(10.0);
        d.cur.top = Some(20.0);
        d.cur.extra.insert("future".into(), serde_json::json!(1));
        d.cur.warn_percent = 70;
        d.cur.topmost = true;
        d.reset_defaults();
        assert_eq!(d.cur.warn_percent, 60);
        assert!(!d.cur.topmost);
        assert_eq!((d.cur.left, d.cur.top), (Some(10.0), Some(20.0)));
        assert!(d.cur.extra.contains_key("future"));
        assert_eq!(d.changes(), 0);
    }
}
