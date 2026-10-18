//! Static textures from the Spec board, each built once and reused: CRT
//! scanlines, the screen vignette and the FEED LOST static.

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions, TextureWrapMode};

pub struct Textures {
    /// 1×3: row 0 black at α 0.22, rows 1–2 clear. Tiled, UV = rect px / 3.
    pub scanlines: TextureHandle,
    /// 64×64: clear to 55% radius, black at α 0.55 at the edge. Stretched.
    pub vignette: TextureHandle,
    /// 128×128 grey noise at α 0.10–0.35. Tiled; UV offset shifts it.
    pub noise: TextureHandle,
}

fn repeat() -> TextureOptions {
    TextureOptions { wrap_mode: TextureWrapMode::Repeat, ..TextureOptions::NEAREST }
}

impl Textures {
    pub fn new(ctx: &egui::Context) -> Self {
        let line = Color32::from_black_alpha((0.22 * 255.0) as u8);
        let scan = ColorImage::new([1, 3], vec![line, Color32::TRANSPARENT, Color32::TRANSPARENT]);

        let n = 64usize;
        let mut vig = Vec::with_capacity(n * n);
        for y in 0..n {
            for x in 0..n {
                // Elliptical distance from the centre, 1.0 at the edge midpoints.
                let dx = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let dy = (y as f32 + 0.5) / n as f32 * 2.0 - 1.0;
                let d = (dx * dx + dy * dy).sqrt() / std::f32::consts::SQRT_2;
                let t = ((d - 0.55) / 0.45).clamp(0.0, 1.0);
                vig.push(Color32::from_black_alpha((t * 0.55 * 255.0) as u8));
            }
        }

        let m = 128usize;
        // Small xorshift: deterministic, no rand crate.
        let mut s: u32 = 0x9E37_79B9;
        let noise = (0..m * m)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let grey = (s & 0xFF) as u8;
                let alpha = (0.10 + ((s >> 8) & 0xFF) as f32 / 255.0 * 0.25) * 255.0;
                Color32::from_rgba_unmultiplied(grey, grey, grey, alpha as u8)
            })
            .collect();

        Self {
            scanlines: ctx.load_texture("scanlines", scan, repeat()),
            vignette: ctx.load_texture("vignette", ColorImage::new([n, n], vig), TextureOptions::LINEAR),
            noise: ctx.load_texture("noise", ColorImage::new([m, m], noise), repeat()),
        }
    }
}
