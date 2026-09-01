//! High-quality terminal logo renderer using PNG half-block characters.

use std::io::{self, Write};

/// Render the Niubash logo to stdout using ANSI 256-color half-block characters.
///
/// `target_cols` controls the width in terminal columns. The height is computed
/// automatically to preserve aspect ratio (each character row = 2 pixel rows).
/// Pass 0 for automatic sizing based on terminal width.
pub fn render_logo(target_cols: u16) {
    let logo_bytes = include_bytes!("../../../assets/niubash-icon-256.png");

    let mut decoder = png::Decoder::new(std::io::Cursor::new(logo_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) {
        Ok(info) => info,
        Err(_) => return,
    };

    let pixels: Vec<(u8, u8, u8)> = match info.color_type {
        png::ColorType::Rgba => buf
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as u16;
                let na = 255 - a;
                (
                    (((p[0] as i32 - 20) * a as i32 + 20 * na as i32) / 255) as u8,
                    (((p[1] as i32 - 22) * a as i32 + 22 * na as i32) / 255) as u8,
                    (((p[2] as i32 - 40) * a as i32 + 40 * na as i32) / 255) as u8,
                )
            })
            .collect(),
        png::ColorType::Rgb => buf.chunks_exact(3).map(|p| (p[0], p[1], p[2])).collect(),
        png::ColorType::Grayscale => buf.iter().map(|&g| (g, g, g)).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).map(|p| (p[0], p[0], p[0])).collect(),
        _ => return,
    };

    let w = info.width as usize;
    let h = info.height as usize;
    if pixels.is_empty() || w == 0 || h == 0 {
        return;
    }

    let tw = if target_cols > 0 {
        target_cols as usize
    } else {
        let term_w = super::interactive_menu::term_width() as usize;
        (term_w / 3).max(20).min(60)
    };
    let th = ((h as f64 / w as f64 * tw as f64 * 0.5) as usize).max(1);

    let resized = resize_pixels(&pixels, w, h, tw, th * 2);

    let mut out = String::with_capacity(tw * th * 20);
    for row in 0..th {
        let y1 = row * 2;
        let y2 = y1 + 1;
        for x in 0..tw {
            let upper = pixel_at(&resized, x, y1, tw, th * 2);
            let lower = if y2 < th * 2 {
                pixel_at(&resized, x, y2, tw, th * 2)
            } else {
                (20, 22, 40)
            };
            let bg = rgb_to_ansi256(upper.0, upper.1, upper.2);
            let fg = rgb_to_ansi256(lower.0, lower.1, lower.2);
            out.push_str(&format!("\x1b[38;5;{fg}m\x1b[48;5;{bg}m▄"));
        }
        out.push_str("\x1b[0m\n");
    }

    // Brand label
    out.push_str("\x1b[1m");
    out.push_str("\x1b[38;5;27m");
    out.push_str("niu");
    out.push_str("\x1b[38;5;214m");
    out.push_str("bash");
    out.push_str("\x1b[0m\n");

    let mut stdout = io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

/// Render the logo to a string and return it (for side-by-side layout use).
pub fn render_logo_to_string(target_cols: u16) -> String {
    let logo_bytes = include_bytes!("../../../assets/niubash-icon-256.png");

    let mut decoder = png::Decoder::new(std::io::Cursor::new(logo_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) {
        Ok(info) => info,
        Err(_) => return String::new(),
    };

    let pixels: Vec<(u8, u8, u8)> = match info.color_type {
        png::ColorType::Rgba => buf
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as u16;
                let na = 255 - a;
                (
                    (((p[0] as i32 - 20) * a as i32 + 20 * na as i32) / 255) as u8,
                    (((p[1] as i32 - 22) * a as i32 + 22 * na as i32) / 255) as u8,
                    (((p[2] as i32 - 40) * a as i32 + 40 * na as i32) / 255) as u8,
                )
            })
            .collect(),
        png::ColorType::Rgb => buf.chunks_exact(3).map(|p| (p[0], p[1], p[2])).collect(),
        png::ColorType::Grayscale => buf.iter().map(|&g| (g, g, g)).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).map(|p| (p[0], p[0], p[0])).collect(),
        _ => return String::new(),
    };

    let w = info.width as usize;
    let h = info.height as usize;
    if pixels.is_empty() || w == 0 || h == 0 {
        return String::new();
    }

    let tw = target_cols as usize;
    let th = ((h as f64 / w as f64 * tw as f64 * 0.5) as usize).max(1);

    let resized = resize_pixels(&pixels, w, h, tw, th * 2);

    let mut out = String::with_capacity(tw * th * 20 + 100);
    for row in 0..th {
        let y1 = row * 2;
        let y2 = y1 + 1;
        for x in 0..tw {
            let upper = pixel_at(&resized, x, y1, tw, th * 2);
            let lower = if y2 < th * 2 {
                pixel_at(&resized, x, y2, tw, th * 2)
            } else {
                (20, 22, 40)
            };
            let bg = rgb_to_ansi256(upper.0, upper.1, upper.2);
            let fg = rgb_to_ansi256(lower.0, lower.1, lower.2);
            out.push_str(&format!("\x1b[38;5;{fg}m\x1b[48;5;{bg}m▄"));
        }
        out.push_str("\x1b[0m\n");
    }

    out.push_str("\x1b[1m");
    out.push_str("\x1b[38;5;27m");
    out.push_str("niu");
    out.push_str("\x1b[38;5;214m");
    out.push_str("bash");
    out.push_str("\x1b[0m\n");

    out
}

/// Return the number of terminal rows the logo occupies at the given column width.
pub fn logo_height(target_cols: u16) -> u16 {
    let logo_bytes = include_bytes!("../../../assets/niubash-icon-256.png");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(logo_bytes));
    let _ = decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) {
        Ok(info) => info,
        Err(_) => return 0,
    };
    let w = info.width as f64;
    let h = info.height as f64;
    let tw = target_cols as f64;
    let th = (h / w * tw * 0.5).max(1.0);
    // image rows + brand label row
    th as u16 + 1
}

fn pixel_at(buf: &[(u8, u8, u8)], x: usize, y: usize, w: usize, h: usize) -> (u8, u8, u8) {
    if x < w && y < h {
        buf[y * w + x]
    } else {
        (20, 22, 40)
    }
}

fn resize_pixels(
    src: &[(u8, u8, u8)],
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
) -> Vec<(u8, u8, u8)> {
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let sy = (y * sh / dh).min(sh - 1);
        for x in 0..dw {
            let sx = (x * sw / dw).min(sw - 1);
            out.push(src[sy * sw + sx]);
        }
    }
    out
}

fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let grey = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    let grey_idx = ((grey as u16 * 23 + 128) / 255) as u8;
    if grey_idx > 0
        && (r as i16 - grey as i16).abs() < 10
        && (g as i16 - grey as i16).abs() < 10
        && (b as i16 - grey as i16).abs() < 10
    {
        return 232 + grey_idx;
    }
    let ri = ((r as u16 * 5 + 128) / 255) as u8;
    let gi = ((g as u16 * 5 + 128) / 255) as u8;
    let bi = ((b as u16 * 5 + 128) / 255) as u8;
    16 + 36 * ri + 6 * gi + bi
}
