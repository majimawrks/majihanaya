//! CPU renderer for egui's meshes: a scanline triangle rasterizer plus a
//! tile cache. The frame is cut into `TILE`×`TILE` tiles; each tile hashes
//! the triangles touching it, and only tiles whose hash changed are drawn
//! again. A blinking chip therefore costs a few tiles, not the whole window.
//!
//! Output is premultiplied BGRA (`0xAARRGGBB` little-endian), the format a
//! layered window's DIB wants. Blending is premultiplied source-over in
//! sRGB space, matching what egui's GPU backends produce.

use egui::epaint::{ClippedPrimitive, Primitive, TextureId, Vertex};
use egui::{ImageData, TextureFilter, TextureWrapMode, TexturesDelta};
use std::collections::HashMap;

pub const TILE: usize = 32;

struct Texture {
    w: usize,
    h: usize,
    /// Premultiplied, `0xAARRGGBB`.
    px: Vec<u32>,
    linear: bool,
    repeat: bool,
    /// Bumped on every update so tiles using it redraw.
    version: u64,
}

/// A triangle in pixel space, ready to draw.
struct Tri {
    v: [Vertex; 3],
    tex: TextureId,
    /// Pixel bounds, already intersected with the clip rect.
    bbox: [i32; 4],
}

#[derive(Default)]
pub struct Renderer {
    textures: HashMap<TextureId, Texture>,
    version: u64,
    size: (usize, usize),
    /// Hash per tile from the last drawn frame; `None` forces a redraw.
    hashes: Vec<Option<u64>>,
    /// Scratch, kept between frames to avoid reallocating.
    tris: Vec<Tri>,
    lists: Vec<Vec<u32>>,
}

/// A dirty region in pixels: x, y, width, height.
pub type Rect = [usize; 4];

fn mix(h: u64, v: u64) -> u64 {
    // A small multiply-xorshift mix (no extra dependency); collisions only
    // risk a missed redraw of one tile until its next change.
    let x = (h ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^ (x >> 29)
}

fn argb(c: [u8; 4]) -> u32 {
    (c[3] as u32) << 24 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

impl Renderer {
    /// Applies new and updated textures. Returns the ids to free, which the
    /// caller passes to `free_textures` only *after* rendering this frame
    /// (a texture can be drawn in the very pass that drops its handle).
    #[must_use]
    pub fn update_textures(&mut self, mut delta: TexturesDelta) -> Vec<TextureId> {
        for (id, deltas) in std::mem::take(&mut delta.set) {
            for d in deltas {
                let ImageData::Color(img) = &d.image;
                self.version += 1;
                let src: Vec<u32> = img.pixels.iter().map(|c| argb(c.to_array())).collect();
                let linear = d.options.magnification == TextureFilter::Linear;
                let repeat = d.options.wrap_mode == TextureWrapMode::Repeat;
                match d.pos {
                    None => {
                        self.textures.insert(
                            id,
                            Texture { w: img.size[0], h: img.size[1], px: src, linear, repeat, version: self.version },
                        );
                    }
                    Some([x0, y0]) => {
                        if let Some(t) = self.textures.get_mut(&id) {
                            t.linear = linear;
                            t.repeat = repeat;
                            let w = img.size[0];
                            for y in 0..img.size[1] {
                                let row = (y0 + y) * t.w + x0;
                                if row + w <= t.px.len() {
                                    t.px[row..row + w].copy_from_slice(&src[y * w..(y + 1) * w]);
                                }
                            }
                            t.version = self.version;
                        }
                    }
                }
            }
        }
        std::mem::take(&mut delta.free).into_iter().collect()
    }

    pub fn free_textures(&mut self, ids: Vec<TextureId>) {
        for id in ids {
            self.textures.remove(&id);
        }
    }

    /// Forces a full redraw next frame (e.g. the target buffer was recreated).
    pub fn invalidate(&mut self) {
        self.hashes.iter_mut().for_each(|h| *h = None);
    }

    /// Draws `prims` into `target` (`w`×`h`, row-major), redrawing only tiles
    /// that changed since the last call. Returns the redrawn tiles.
    pub fn render(&mut self, target: &mut [u32], w: usize, h: usize, prims: &[ClippedPrimitive], ppp: f32) -> Vec<Rect> {
        assert_eq!(target.len(), w * h, "target size");
        let (tw, th) = (w.div_ceil(TILE), h.div_ceil(TILE));
        if self.size != (w, h) {
            self.size = (w, h);
            self.hashes = vec![None; tw * th];
        }
        self.lists.resize_with(tw * th, Vec::new);
        self.lists.iter_mut().for_each(Vec::clear);
        self.tris.clear();
        let mut new_hash = vec![0x51_7C_C1_B7_27_22_0A_95u64; tw * th];

        for cp in prims {
            let Primitive::Mesh(mesh) = &cp.primitive else { continue };
            let Some(tex) = self.textures.get(&mesh.texture_id) else { continue };
            let c = cp.clip_rect;
            let clip = [
                (c.min.x * ppp).round().max(0.0) as i32,
                (c.min.y * ppp).round().max(0.0) as i32,
                ((c.max.x * ppp).round() as i32).min(w as i32),
                ((c.max.y * ppp).round() as i32).min(h as i32),
            ];
            if clip[0] >= clip[2] || clip[1] >= clip[3] {
                continue;
            }
            let tex_key = mix(tex.version, mesh.texture_id_hash());
            for idx in mesh.indices.chunks_exact(3) {
                let mut v = [mesh.vertices[idx[0] as usize], mesh.vertices[idx[1] as usize], mesh.vertices[idx[2] as usize]];
                for vx in &mut v {
                    vx.pos.x *= ppp;
                    vx.pos.y *= ppp;
                }
                let xs = [v[0].pos.x, v[1].pos.x, v[2].pos.x];
                let ys = [v[0].pos.y, v[1].pos.y, v[2].pos.y];
                let bbox = [
                    (xs.iter().copied().fold(f32::MAX, f32::min).floor() as i32).max(clip[0]),
                    (ys.iter().copied().fold(f32::MAX, f32::min).floor() as i32).max(clip[1]),
                    (xs.iter().copied().fold(f32::MIN, f32::max).ceil() as i32).min(clip[2]),
                    (ys.iter().copied().fold(f32::MIN, f32::max).ceil() as i32).min(clip[3]),
                ];
                if bbox[0] >= bbox[2] || bbox[1] >= bbox[3] {
                    continue;
                }
                let mut th_ = tex_key;
                for vx in &v {
                    th_ = mix(th_, (vx.pos.x.to_bits() as u64) << 32 | vx.pos.y.to_bits() as u64);
                    th_ = mix(th_, (vx.uv.x.to_bits() as u64) << 32 | vx.uv.y.to_bits() as u64);
                    th_ = mix(th_, u32::from_le_bytes(vx.color.to_array()) as u64);
                }
                th_ = mix(th_, (clip[0] as u64) << 48 ^ (clip[1] as u64) << 32 ^ (clip[2] as u64) << 16 ^ clip[3] as u64);
                let i = self.tris.len() as u32;
                self.tris.push(Tri { v, tex: mesh.texture_id, bbox });
                let (tx0, ty0) = (bbox[0] as usize / TILE, bbox[1] as usize / TILE);
                let (tx1, ty1) = ((bbox[2] as usize - 1) / TILE, (bbox[3] as usize - 1) / TILE);
                for ty in ty0..=ty1.min(th - 1) {
                    for tx in tx0..=tx1.min(tw - 1) {
                        let t = ty * tw + tx;
                        self.lists[t].push(i);
                        new_hash[t] = mix(new_hash[t], th_);
                    }
                }
            }
        }

        let mut dirty = Vec::new();
        for t in 0..tw * th {
            if self.hashes[t] == Some(new_hash[t]) {
                continue;
            }
            self.hashes[t] = Some(new_hash[t]);
            let (x0, y0) = ((t % tw) * TILE, (t / tw) * TILE);
            let rect = [x0, y0, TILE.min(w - x0), TILE.min(h - y0)];
            for y in y0..y0 + rect[3] {
                target[y * w + x0..y * w + x0 + rect[2]].fill(0);
            }
            let r = [x0 as i32, y0 as i32, (x0 + rect[2]) as i32, (y0 + rect[3]) as i32];
            for &i in &self.lists[t] {
                let tri = &self.tris[i as usize];
                if let Some(tex) = self.textures.get(&tri.tex) {
                    raster(target, w, tri, tex, r);
                }
            }
            dirty.push(rect);
        }
        dirty
    }
}

trait TexIdHash {
    fn texture_id_hash(&self) -> u64;
}

impl TexIdHash for egui::epaint::Mesh {
    fn texture_id_hash(&self) -> u64 {
        match self.texture_id {
            TextureId::Managed(n) => n,
            TextureId::User(n) => n ^ 1 << 63,
        }
    }
}

fn sample(t: &Texture, u: f32, v: f32) -> [f32; 4] {
    let fetch = |x: i64, y: i64| -> u32 {
        let (x, y) = if t.repeat {
            (x.rem_euclid(t.w as i64) as usize, y.rem_euclid(t.h as i64) as usize)
        } else {
            (x.clamp(0, t.w as i64 - 1) as usize, y.clamp(0, t.h as i64 - 1) as usize)
        };
        t.px[y * t.w + x]
    };
    let unpack = |p: u32| [(p >> 16 & 0xFF) as f32, (p >> 8 & 0xFF) as f32, (p & 0xFF) as f32, (p >> 24) as f32];
    let (fx, fy) = (u * t.w as f32, v * t.h as f32);
    if !t.linear {
        return unpack(fetch(fx.floor() as i64, fy.floor() as i64));
    }
    let (x, y) = (fx - 0.5, fy - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (ax, ay) = (x - x0, y - y0);
    let (xi, yi) = (x0 as i64, y0 as i64);
    let (a, b, c, d) = (unpack(fetch(xi, yi)), unpack(fetch(xi + 1, yi)), unpack(fetch(xi, yi + 1)), unpack(fetch(xi + 1, yi + 1)));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * ax;
        let bot = c[i] + (d[i] - c[i]) * ax;
        top + (bot - top) * ay
    })
}

#[inline]
fn blend(dst: &mut u32, s: [f32; 4]) {
    let sa = s[3].clamp(0.0, 255.0);
    if sa <= 0.0 && s[0] <= 0.0 && s[1] <= 0.0 && s[2] <= 0.0 {
        return;
    }
    let k = 1.0 - sa / 255.0;
    let d = *dst;
    let ch = |src: f32, shift: u32| -> u32 { (src + ((d >> shift) & 0xFF) as f32 * k + 0.5).clamp(0.0, 255.0) as u32 };
    *dst = ch(sa, 24) << 24 | ch(s[0], 16) << 16 | ch(s[1], 8) << 8 | ch(s[2], 0);
}

/// Fills `tri` within `r` (pixel rect [x0, y0, x1, y1)) and its clip, sampling
/// pixel centres. Barycentric weights and attributes step incrementally.
fn raster(target: &mut [u32], w: usize, tri: &Tri, tex: &Texture, r: [i32; 4]) {
    let x0 = tri.bbox[0].max(r[0]);
    let y0 = tri.bbox[1].max(r[1]);
    let x1 = tri.bbox[2].min(r[2]);
    let y1 = tri.bbox[3].min(r[3]);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let [a, b, c] = &tri.v;
    // Work relative to vertex a: keeps the edge maths precise far from the
    // window origin (no cancellation between large coordinates).
    let (bx, by) = (b.pos.x - a.pos.x, b.pos.y - a.pos.y);
    let (cx, cy) = (c.pos.x - a.pos.x, c.pos.y - a.pos.y);
    let area = bx * cy - cx * by;
    if area.abs() < 1e-8 {
        return;
    }
    let inv = 1.0 / area;
    // Barycentric weights (positive inside whatever the winding), with
    // their exact derivatives: wb = (x·cy − y·cx)/area, wa likewise, wc = 1 − wa − wb.
    let wa = |x: f32, y: f32| ((bx - x) * (cy - y) - (cx - x) * (by - y)) * inv;
    let wb = |x: f32, y: f32| (x * cy - y * cx) * inv;
    let (dwa_dx, dwa_dy) = ((by - cy) * inv, (cx - bx) * inv);
    let (dwb_dx, dwb_dy) = (cy * inv, -cx * inv);
    let (dwc_dx, dwc_dy) = (-dwa_dx - dwb_dx, -dwa_dy - dwb_dy);
    // Top-left fill rule: a pixel centre exactly on an edge belongs to the
    // triangle only if that edge is a top or left edge, so two triangles
    // sharing an edge never both paint it (no double-blended seams).
    let owns = |gx: f32, gy: f32| if gx.abs() < 1e-12 { gy > 0.0 } else { gx > 0.0 };
    let incl = [owns(dwa_dx, dwa_dy), owns(dwb_dx, dwb_dy), owns(dwc_dx, dwc_dy)];

    let col = |v: &Vertex| {
        let c = v.color.to_array();
        [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]
    };
    let (ca, cb, cc) = (col(a), col(b), col(c));
    let uv_const = a.uv == b.uv && b.uv == c.uv;
    let col_const = ca == cb && cb == cc;
    let texel_const = uv_const.then(|| sample(tex, a.uv.x, a.uv.y));
    // Fully constant source: vertex colour × one texel.
    let src_const = (uv_const && col_const).then(|| {
        let t = texel_const.unwrap();
        std::array::from_fn::<f32, 4, _>(|i| ca[i] * t[i] / 255.0)
    });

    // Weights within TOL of 0 count as "on the edge" (float noise).
    const TOL: f32 = 1e-6;
    let sx = x0 as f32 + 0.5 - a.pos.x;
    for y in y0..y1 {
        // Evaluated fresh per row (no accumulated error down tall triangles).
        let sy = y as f32 + 0.5 - a.pos.y;
        let (row_a, row_b) = (wa(sx, sy), wb(sx, sy));
        // Span where every weight passes its edge test, from the row's linear forms.
        let (mut lo, mut hi) = (x0 as f32, x1 as f32);
        for (i, (w0, d)) in [(row_a, dwa_dx), (row_b, dwb_dx), (1.0 - row_a - row_b, dwc_dx)].into_iter().enumerate() {
            // Owned edges keep w ≥ 0; others need w > 0.
            let thr = if incl[i] { -TOL } else { TOL };
            if d.abs() < 1e-12 {
                if w0 < thr {
                    hi = lo; // no pixel on this row
                }
            } else {
                let t = (thr - w0) / d; // pixels from x0 where the weight crosses thr
                if d > 0.0 {
                    lo = lo.max(x0 as f32 + t.ceil());
                } else {
                    hi = hi.min(x0 as f32 + t.floor() + 1.0);
                }
            }
        }
        let (xs, xe) = (lo.max(x0 as f32) as i32, hi.min(x1 as f32) as i32);
        if xs < xe {
            let row = &mut target[y as usize * w..y as usize * w + w];
            match src_const {
                Some(s) => {
                    for px in &mut row[xs as usize..xe as usize] {
                        blend(px, s);
                    }
                }
                None => {
                    let k = (xs - x0) as f32;
                    let (mut wa_, mut wb_) = (row_a + dwa_dx * k, row_b + dwb_dx * k);
                    for px in &mut row[xs as usize..xe as usize] {
                        let wc_ = 1.0 - wa_ - wb_;
                        let vc: [f32; 4] = if col_const { ca } else { std::array::from_fn(|i| wa_ * ca[i] + wb_ * cb[i] + wc_ * cc[i]) };
                        let t = match texel_const {
                            Some(t) => t,
                            None => sample(
                                tex,
                                wa_ * a.uv.x + wb_ * b.uv.x + wc_ * c.uv.x,
                                wa_ * a.uv.y + wb_ * b.uv.y + wc_ * c.uv.y,
                            ),
                        };
                        blend(px, std::array::from_fn(|i| vc[i] * t[i] / 255.0));
                        wa_ += dwa_dx;
                        wb_ += dwb_dx;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{Mesh, Pos2, Rect as ERect};
    use egui::epaint::ImageDelta;
    use egui::{Color32, ColorImage, TextureOptions};

    fn white_tex(r: &mut Renderer) {
        let mut d = TexturesDelta::default();
        let img = ColorImage::new([1, 1], vec![Color32::WHITE]);
        d.set.insert(TextureId::Managed(0), [ImageDelta::full(img, TextureOptions::NEAREST)].into_iter().collect());
        let _ = r.update_textures(d);
    }

    fn rect_mesh(x0: f32, y0: f32, x1: f32, y1: f32, c: Color32) -> ClippedPrimitive {
        let mut m = Mesh::default();
        m.add_colored_rect(ERect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1)), c);
        ClippedPrimitive { clip_rect: ERect::EVERYTHING, primitive: Primitive::Mesh(m) }
    }

    #[test]
    fn fills_exact_pixels_and_blends() {
        let mut r = Renderer::default();
        white_tex(&mut r);
        let (w, h) = (64, 40);
        let mut buf = vec![0u32; w * h];
        let prims = [rect_mesh(10.0, 10.0, 20.0, 15.0, Color32::from_rgb(255, 0, 0)), rect_mesh(15.0, 12.0, 25.0, 20.0, Color32::from_black_alpha(128))];
        r.render(&mut buf, w, h, &prims, 1.0);
        assert_eq!(buf[10 * w + 10], 0xFFFF_0000);
        assert_eq!(buf[14 * w + 14], 0xFFFF_0000);
        assert_eq!(buf[15 * w + 10], 0, "bottom edge exclusive");
        assert_eq!(buf[10 * w + 20], 0, "right edge exclusive");
        // Red under 50% black: alpha stays 255, red halves.
        let p = buf[12 * w + 16];
        assert_eq!(p >> 24, 255);
        assert!(((p >> 16) & 0xFF).abs_diff(127) <= 1, "{p:08x}");
        // 50% black over transparent: premultiplied (0,0,0,128).
        assert_eq!(buf[18 * w + 22], 0x8000_0000);
    }

    fn tri_mesh(p: [(f32, f32); 3], c: Color32) -> ClippedPrimitive {
        let mut m = Mesh::default();
        for (x, y) in p {
            m.colored_vertex(Pos2::new(x, y), c);
        }
        m.add_triangle(0, 1, 2);
        ClippedPrimitive { clip_rect: ERect::EVERYTHING, primitive: Primitive::Mesh(m) }
    }

    #[test]
    fn shared_edges_are_painted_once() {
        // A translucent square split on its diagonal: the diagonal passes
        // through pixel centres, which must not be blended twice.
        let mut r = Renderer::default();
        white_tex(&mut r);
        let (w, h) = (16, 16);
        let mut buf = vec![0u32; w * h];
        let c = Color32::from_black_alpha(128);
        let prims = [rect_mesh(2.0, 2.0, 10.0, 10.0, c)];
        r.render(&mut buf, w, h, &prims, 1.0);
        for y in 2..10 {
            for x in 2..10 {
                assert_eq!(buf[y * w + x], 0x8000_0000, "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn small_triangles_far_from_origin_keep_their_pixels() {
        // From review: cancellation in the derivatives dropped (508, 500).
        let mut r = Renderer::default();
        white_tex(&mut r);
        let (w, h) = (520, 510);
        let mut buf = vec![0u32; w * h];
        let prims = [tri_mesh([(500.1, 500.2), (510.3, 500.2), (500.1, 501.9)], Color32::WHITE)];
        r.render(&mut buf, w, h, &prims, 1.0);
        assert_eq!(buf[500 * w + 508], 0xFFFF_FFFF);
    }

    #[test]
    fn unchanged_frame_redraws_nothing_and_a_change_redraws_its_tiles() {
        let mut r = Renderer::default();
        white_tex(&mut r);
        let (w, h) = (128, 96);
        let mut buf = vec![0u32; w * h];
        let a = rect_mesh(4.0, 4.0, 120.0, 90.0, Color32::DARK_GRAY);
        let chip = |c| rect_mesh(70.0, 70.0, 80.0, 80.0, c);
        assert_eq!(r.render(&mut buf, w, h, &[a.clone(), chip(Color32::RED)], 1.0).len(), 4 * 3);
        assert!(r.render(&mut buf, w, h, &[a.clone(), chip(Color32::RED)], 1.0).is_empty());
        let dirty = r.render(&mut buf, w, h, &[a, chip(Color32::GREEN)], 1.0);
        assert_eq!(dirty, vec![[64, 64, 32, 32]]);
        assert_eq!(buf[75 * w + 75], argb(Color32::GREEN.to_array()));
    }

    #[test]
    fn texture_update_redraws_users() {
        let mut r = Renderer::default();
        white_tex(&mut r);
        let (w, h) = (64, 64);
        let mut buf = vec![0u32; w * h];
        let p = [rect_mesh(0.0, 0.0, 10.0, 10.0, Color32::WHITE)];
        r.render(&mut buf, w, h, &p, 1.0);
        let mut d = TexturesDelta::default();
        d.set.insert(
            TextureId::Managed(0),
            [ImageDelta::full(ColorImage::new([1, 1], vec![Color32::BLUE]), TextureOptions::NEAREST)].into_iter().collect(),
        );
        let _ = r.update_textures(d);
        assert_eq!(r.render(&mut buf, w, h, &p, 1.0).len(), 1);
        assert_eq!(buf[5 * w + 5], argb(Color32::BLUE.to_array()));
    }
}
