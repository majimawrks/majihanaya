//! Colours and type from the design's Spec board ("the Florist's monitor room").

use egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// One phosphor level: number and lit cells, labels, screen, unlit cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Phosphor {
    pub fg: Color32,
    pub dim: Color32,
    pub bg: Color32,
    pub off: Color32,
    /// Whether the big number gets the glow pass.
    pub glow: bool,
}

pub const OK: Phosphor = Phosphor { fg: hex(0x4ADE80), dim: hex(0x2F8F57), bg: hex(0x06120A), off: hex(0x0F2817), glow: true };
pub const WARN: Phosphor = Phosphor { fg: hex(0xFBBF24), dim: hex(0xB08A1E), bg: hex(0x130E04), off: hex(0x2A1F08), glow: true };
pub const CRIT: Phosphor = Phosphor { fg: hex(0xF87171), dim: hex(0xD06262), bg: hex(0x150707), off: hex(0x2E1111), glow: true };
pub const NO_DATA: Phosphor = Phosphor { fg: hex(0x6B7280), dim: hex(0x7E8593), bg: hex(0x0B0C0E), off: hex(0x181A1F), glow: false };
pub const STALE: Phosphor = Phosphor { fg: hex(0x7C8A80), dim: hex(0x75827A), bg: hex(0x0A0D0B), off: hex(0x161C18), glow: false };
pub const LOST: Phosphor = Phosphor { fg: hex(0xD9DCD3), dim: hex(0x8B9086), bg: hex(0x0A0A0B), off: hex(0x161616), glow: false };

/// Chrome (Spec board, "CHROME").
pub mod chrome {
    use super::hex;
    use egui::Color32;
    pub const RACK: Color32 = hex(0x0C0D0B);
    pub const BORDER: Color32 = hex(0x2B2D28);
    pub const BEZEL: Color32 = hex(0x1B1C19);
    pub const BEZEL_TOP: Color32 = hex(0x34372F);
    pub const BEZEL_BOTTOM: Color32 = hex(0x050605);
    pub const PLATE: Color32 = hex(0x22241F);
    pub const INK: Color32 = hex(0xD9DCD3);
    pub const PLATE_TEXT: Color32 = hex(0xA8AD9F);
    pub const MUTED: Color32 = hex(0x7A7F74);
    pub const SUBTLE: Color32 = hex(0x8B9086);
    pub const REC_ON: Color32 = hex(0xF25555);
    pub const REC_OFF: Color32 = hex(0x5A2323);
    pub const NOTICE_BG: Color32 = hex(0x2A1F08);
    pub const NOTICE_BORDER: Color32 = hex(0x6B5212);
    pub const AMBER: Color32 = hex(0xFBBF24);
    pub const GROUP_BG: Color32 = hex(0x10110F);
    pub const BORDER_HI: Color32 = hex(0x3A3D36);
    pub const SELECTED_BG: Color32 = hex(0x0F2817);
    pub const GREEN: Color32 = hex(0x4ADE80);
    pub const GREEN_DIM: Color32 = hex(0x2F8F57);
}

const VT323: &str = "vt323";
const MONO: &str = "jetbrains-mono";

/// VT323: the burn-in layer (numbers, labels). Falls back to JetBrains Mono
/// for glyphs it lacks (● ◆ ▲ ↑ …).
pub fn vt(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(VT323.into()))
}

/// JetBrains Mono: small prose (session names, emails, notices).
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Only these two fonts are loaded; egui's defaults are compiled out.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();
    fonts
        .font_data
        .insert(VT323.into(), Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/VT323-Regular.ttf"))));
    fonts
        .font_data
        .insert(MONO.into(), Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"))));
    fonts.families.insert(FontFamily::Name(VT323.into()), vec![VT323.into(), MONO.into()]);
    fonts.families.insert(FontFamily::Monospace, vec![MONO.into()]);
    fonts.families.insert(FontFamily::Proportional, vec![MONO.into()]);
    ctx.set_fonts(fonts);

    // Both themes: the look never follows the system light/dark setting.
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = chrome::RACK;
        v.override_text_color = Some(chrome::INK);
        // Spec: focus ring 1 px green.
        v.selection.stroke = egui::Stroke::new(1.0, chrome::GREEN);
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, chrome::BORDER_HI);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.interaction.selectable_labels = false;
    });
}
