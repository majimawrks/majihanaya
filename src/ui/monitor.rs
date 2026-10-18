//! One CCTV monitor: bezel, CRT screen, and either a usage feed or any
//! content drawn by the caller. Geometry from the Spec board (logical px).

use super::feeds::{Extra, Feed, Mode};
use super::textures::Textures;
use super::theme::{self, Phosphor, chrome, vt};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, epaint, pos2, vec2,
};
use eframe::egui::text::{LayoutJob, TextFormat};

pub const BEZEL_PAD: f32 = 5.0;
pub const SCREEN_PAD: Vec2 = vec2(10.0, 6.0);

/// Paints text with optional letter spacing; returns its rect.
pub fn text(p: &Painter, pos: Pos2, align: Align2, s: &str, font: FontId, color: Color32, spacing: f32) -> Rect {
    let mut job = LayoutJob::default();
    job.append(s, 0.0, TextFormat { font_id: font, color, extra_letter_spacing: spacing, ..Default::default() });
    let galley = p.layout_job(job);
    let rect = align.anchor_size(pos, galley.size());
    p.galley(rect.min, galley, color);
    rect
}

/// The big number's glow: four copies offset ±1 px at 22% alpha, then crisp.
fn glow_text(p: &Painter, pos: Pos2, align: Align2, s: &str, font: FontId, ph: &Phosphor) -> Rect {
    if ph.glow {
        let halo = ph.fg.gamma_multiply(0.22);
        for d in [vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0)] {
            text(p, pos + d, align, s, font.clone(), halo, 0.0);
        }
    }
    text(p, pos, align, s, font, ph.fg, 0.0)
}

/// 20 cells, 5% each, gap 2, radius 1.
pub fn cell_bar(p: &Painter, rect: Rect, percent: Option<f32>, ph: &Phosphor) {
    let lit = percent.map_or(0, |v| (v.clamp(0.0, 100.0) / 5.0).round() as usize);
    let gap = 2.0;
    let w = (rect.width() - gap * 19.0) / 20.0;
    for i in 0..20 {
        let x = rect.left() + i as f32 * (w + gap);
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
        p.rect_filled(cell, 1.0, if i < lit { ph.fg } else { ph.off });
    }
}

/// Animation inputs for one screen this frame.
#[derive(Clone, Copy, Default)]
pub struct Fx {
    /// Refresh sweep progress 0..1 (Spec: 18 px roll bar over 400 ms).
    pub sweep: Option<f32>,
    /// Static burst frame (shifts the noise UV) when a feed changes state.
    pub burst: Option<u32>,
}

/// Bezel + screen of `screen_h`; `content` draws inside the padded screen.
pub fn monitor(
    ui: &mut egui::Ui,
    tex: &Textures,
    width: f32,
    screen_h: f32,
    screen_bg: Color32,
    bezel_stroke: Option<Color32>,
    fx: Fx,
    content: impl FnOnce(&Painter, Rect),
) -> Rect {
    let (outer, _) = ui.allocate_exact_size(vec2(width, screen_h + BEZEL_PAD * 2.0), egui::Sense::hover());
    let p = ui.painter_at(outer.expand(1.0));
    p.rect_filled(outer, 8.0, chrome::BEZEL);
    match bezel_stroke {
        // WAIT: the whole bezel flashes amber.
        Some(c) => {
            p.rect_stroke(outer, 8.0, Stroke::new(1.0, c), StrokeKind::Inside);
        }
        None => {
            p.hline(outer.x_range().shrink(6.0), outer.top() + 0.5, Stroke::new(1.0, chrome::BEZEL_TOP));
            p.hline(outer.x_range().shrink(6.0), outer.bottom() - 0.5, Stroke::new(1.0, chrome::BEZEL_BOTTOM));
        }
    }
    let screen = outer.shrink(BEZEL_PAD);
    let sp = ui.painter_at(screen);
    sp.rect_filled(screen, 5.0, screen_bg);
    content(&sp, screen.shrink2(SCREEN_PAD));
    overlays(&sp, tex, screen, fx, None);
    outer
}

/// Scanlines, sweep, vignette (and static when `lost`), clipped to `screen`.
fn overlays(p: &Painter, tex: &Textures, screen: Rect, fx: Fx, sweep_color: Option<Color32>) {
    let radius = CornerRadius::same(5);
    if let Some(frame) = fx.burst {
        let off = frame as f32 * 0.37;
        let uv = Rect::from_min_size(pos2(off, off * 1.7), vec2(screen.width() / 128.0, screen.height() / 128.0));
        p.add(epaint::RectShape::filled(screen, radius, Color32::WHITE).with_texture(tex.noise.id(), uv));
    }
    let uv = Rect::from_min_size(Pos2::ZERO, vec2(1.0, screen.height() / 3.0));
    p.add(epaint::RectShape::filled(screen, radius, Color32::WHITE).with_texture(tex.scanlines.id(), uv));
    if let (Some(t), Some(c)) = (fx.sweep, sweep_color) {
        // An 18 px band rolls from above the screen to below it.
        let top = screen.top() - 18.0 + t * (screen.height() + 18.0);
        let band = Rect::from_min_size(pos2(screen.left(), top), vec2(screen.width(), 18.0));
        let mut mesh = epaint::Mesh::default();
        mesh.colored_vertex(band.left_top(), c.gamma_multiply(0.0));
        mesh.colored_vertex(band.right_top(), c.gamma_multiply(0.0));
        mesh.colored_vertex(band.right_bottom(), c.gamma_multiply(0.24));
        mesh.colored_vertex(band.left_bottom(), c.gamma_multiply(0.24));
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        p.add(mesh);
        p.hline(band.x_range(), band.bottom(), Stroke::new(1.0, c.gamma_multiply(0.55)));
    }
    p.add(
        epaint::RectShape::filled(screen, radius, Color32::WHITE)
            .with_texture(tex.vignette.id(), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0))),
    );
}

/// A usage feed monitor (CAM 01/02/03): label row, big %, resets, cell bar,
/// per-model extras, and the NO SIGNAL / FEED LOST / ACQUIRING states.
pub fn feed_monitor(ui: &mut egui::Ui, tex: &Textures, width: f32, f: &Feed, fx: Fx) -> Rect {
    let h = f.screen_height();
    let (outer, _) = ui.allocate_exact_size(vec2(width, h + BEZEL_PAD * 2.0), egui::Sense::hover());
    let p = ui.painter_at(outer.expand(1.0));
    p.rect_filled(outer, 8.0, chrome::BEZEL);
    p.hline(outer.x_range().shrink(6.0), outer.top() + 0.5, Stroke::new(1.0, chrome::BEZEL_TOP));
    p.hline(outer.x_range().shrink(6.0), outer.bottom() - 0.5, Stroke::new(1.0, chrome::BEZEL_BOTTOM));
    let screen = outer.shrink(BEZEL_PAD);
    let sp = ui.painter_at(screen);
    sp.rect_filled(screen, 5.0, f.ph.bg);
    let lost = matches!(f.mode, Mode::Lost { .. });
    if lost {
        // FEED LOST: steady static under the text (no burst on top of it).
        let uv = Rect::from_min_size(Pos2::ZERO, vec2(screen.width() / 128.0, screen.height() / 128.0));
        sp.add(epaint::RectShape::filled(screen, 5, Color32::WHITE).with_texture(tex.noise.id(), uv));
    }
    feed_content(&sp, screen.shrink2(SCREEN_PAD), f);
    let fx = if lost { Fx { burst: None, ..fx } } else { fx };
    overlays(&sp, tex, screen, fx, Some(f.ph.fg));
    outer
}

fn feed_content(p: &Painter, r: Rect, f: &Feed) {
    let ph = &f.ph;
    // Row 1 (16): "CAM 01 · CLAUDE 5H" and the burn-in tag.
    text(p, r.left_top(), Align2::LEFT_TOP, &format!("{} · {}", f.cam, f.title), vt(17.0), ph.dim, 0.0);
    text(p, r.right_top(), Align2::RIGHT_TOP, &f.tag, vt(17.0), f.tag_color, 0.0);

    if let Mode::Lost { why } = &f.mode {
        let c = r.center() + vec2(0.0, 8.0);
        let label = |pos: Pos2, s: &str, font: FontId, color: Color32, spacing: f32| {
            let galley_rect = text(p, pos, Align2::CENTER_CENTER, s, font.clone(), Color32::TRANSPARENT, spacing);
            p.rect_filled(galley_rect.expand2(vec2(8.0, 0.0)), 0.0, Color32::from_black_alpha(178));
            text(p, pos, Align2::CENTER_CENTER, s, font, color, spacing);
        };
        label(c - vec2(0.0, 9.0), "FEED LOST", vt(32.0), chrome::INK, 4.0);
        label(c + vec2(0.0, 14.0), why, vt(16.0), chrome::PLATE_TEXT, 0.0);
        return;
    }

    // Row 2 (40): big number + "%", resets right-aligned.
    let value_top = r.top() + 20.0;
    let base = pos2(r.left(), value_top + 40.0);
    let num = f.percent.map_or("--".into(), |v| format!("{}", v.round() as i32));
    let nr = glow_text(p, base + vec2(0.0, 6.0), Align2::LEFT_BOTTOM, &num, vt(52.0), ph);
    text(p, pos2(nr.right(), base.y - 2.0), Align2::LEFT_BOTTOM, "%", vt(26.0), ph.fg, 0.0);
    text(p, pos2(r.right(), value_top + 6.0), Align2::RIGHT_TOP, &f.reset_a, vt(18.0), ph.fg, 0.0);
    text(p, pos2(r.right(), value_top + 23.0), Align2::RIGHT_TOP, &f.reset_b, vt(18.0), ph.dim, 0.0);

    // Row 3 (6): cell bar, or the NO SIGNAL band in its place.
    let bar_top = value_top + 44.0;
    if let Mode::Stale { frozen_at } = &f.mode {
        let band = Rect::from_min_size(pos2(r.left() - 4.0, bar_top), vec2(r.width() + 8.0, 16.0));
        p.rect_filled(band, 2.0, Color32::from_black_alpha(217));
        p.rect_stroke(band, 2.0, Stroke::new(1.0, chrome::BORDER_HI), StrokeKind::Inside);
        text(p, band.center(), Align2::CENTER_CENTER, &format!("▮▮ NO SIGNAL · {frozen_at}"), vt(17.0), chrome::INK, 2.0);
    } else {
        cell_bar(p, Rect::from_min_size(pos2(r.left(), bar_top), vec2(r.width(), 6.0)), f.percent, ph);
    }

    // Extras (14 + 2 margin each): "FABLE WK  18%  ▮▮▮▮…"
    let mut y = bar_top + 6.0 + 6.0;
    for x in &f.extras {
        extra_row(p, Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), 14.0)), x);
        y += 18.0;
    }
}

fn extra_row(p: &Painter, row: Rect, x: &Extra) {
    let mid = row.center().y;
    text(p, pos2(row.left(), mid), Align2::LEFT_CENTER, &x.label, vt(16.0), x.ph.dim, 0.0);
    let pct = x.percent.map_or("--%".into(), |v| format!("{}%", v.round() as i32));
    text(p, pos2(row.left() + 72.0 + 34.0, mid), Align2::RIGHT_CENTER, &pct, vt(16.0), x.ph.fg, 0.0);
    let bar = Rect::from_min_max(pos2(row.left() + 72.0 + 34.0 + 8.0, mid - 2.0), pos2(row.right(), mid + 2.0));
    cell_bar(p, bar, x.percent, &x.ph);
}

/// The 18 px label plate above each section: "CLAUDE ·· MAX".
pub fn plate(ui: &mut egui::Ui, width: f32, left: &str, right: &str, right_color: Color32, extra: Option<&str>) {
    let (r, _) = ui.allocate_exact_size(vec2(width, 18.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 3.0, chrome::PLATE);
    let lr = text(p, pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, left, vt(17.0), chrome::PLATE_TEXT, 2.0);
    if let Some(e) = extra {
        text(p, pos2(lr.right() + 6.0, r.center().y + 1.0), Align2::LEFT_CENTER, e, theme::mono(10.0), chrome::SUBTLE, 0.0);
    }
    text(p, pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, right, vt(17.0), right_color, 2.0);
}

/// Amber notice strip: "▲ not logged in · run claude auth login".
pub fn notice(ui: &mut egui::Ui, width: f32, msg: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(width, 22.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 4.0, chrome::NOTICE_BG);
    p.rect_stroke(r, 4.0, Stroke::new(1.0, chrome::NOTICE_BORDER), StrokeKind::Inside);
    let tr = text(p, pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, "▲", vt(16.0), chrome::AMBER, 0.0);
    text(p, pos2(tr.right() + 6.0, r.center().y), Align2::LEFT_CENTER, msg, theme::mono(11.0), chrome::AMBER, 0.0);
}
