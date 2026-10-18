//! Regenerates `assets/majihanaya.ico`. Run from the repo root:
//! `cargo run --manifest-path tools/icongen/Cargo.toml`
//!
//! Sizes follow the design (app icon board): 256/96/64 from the master
//! drawing, 48/32/24 from the 32-grid drawing, 16 from the hand-placed map.

use resvg::{tiny_skia, usvg};
use std::fs;

fn render_svg(path: &str, size: u32) -> Vec<u8> {
    let data = fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("valid svg");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().unwrap()
}

fn render_map(path: &str) -> Vec<u8> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let rows: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(rows.len(), 16, "16 rows");
    let mut pixmap = tiny_skia::Pixmap::new(16, 16).unwrap();
    for (y, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), 16, "row {y} must be 16 wide");
        for (x, ch) in row.chars().enumerate() {
            let rgb: u32 = match ch {
                'r' => 0x4A4D45,
                'b' => 0x2B2D28,
                's' => 0x06120A,
                'g' => 0x4ADE80,
                'd' => 0x2F8F57,
                'x' => 0xF25555,
                'k' => 0x34372F,
                _ => continue,
            };
            let c = tiny_skia::PremultipliedColorU8::from_rgba((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
                .unwrap();
            pixmap.pixels_mut()[y * 16 + x] = c;
        }
    }
    pixmap.encode_png().unwrap()
}

/// ICO container with PNG-compressed entries (supported since Windows Vista).
fn write_ico(path: &str, images: &[(u32, Vec<u8>)]) {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    fs::write(path, out).unwrap();
}

fn main() {
    let mut images = Vec::new();
    for size in [256, 96, 64] {
        images.push((size, render_svg("assets/icon-256.svg", size)));
    }
    for size in [48, 32, 24] {
        images.push((size, render_svg("assets/icon-32.svg", size)));
    }
    images.push((16, render_map("assets/icon-16.txt")));
    write_ico("assets/majihanaya.ico", &images);
    // Previews for eyeballing.
    for (size, png) in &images {
        fs::write(format!("assets/preview-{size}.png"), png).unwrap();
    }
    println!("wrote assets/majihanaya.ico ({} sizes)", images.len());
}
