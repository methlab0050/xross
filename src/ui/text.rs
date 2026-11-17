//! Text rendering for compositor UI (window titles in the switcher), with ab_glyph.
//!
//! The font comes from fontconfig (`fc-match sans-serif`), falling back to a search
//! of the usual font directories. Without a font, text simply isn't drawn.

use std::{path::PathBuf, process::Command};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};

pub struct TextRenderer {
    font: Option<FontVec>,
}

/// A rendered line: premultiplied ARGB8888 (little-endian B, G, R, A), `scale`x size.
pub struct TextImage {
    pub pixels: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

impl TextRenderer {
    pub fn new() -> Self {
        let font = find_font().and_then(|path| {
            let data = std::fs::read(&path).ok()?;
            match FontVec::try_from_vec(data) {
                Ok(f) => {
                    tracing::debug!(?path, "loaded UI font");
                    Some(f)
                }
                Err(err) => {
                    tracing::warn!(?path, %err, "unusable UI font");
                    None
                }
            }
        });
        if font.is_none() {
            tracing::warn!("no UI font found; window titles won't be drawn");
        }
        Self { font }
    }

    /// Render `text` at `px` logical pixels times `scale`, truncated with an ellipsis to
    /// `max_width` logical pixels.
    pub fn render(&self, text: &str, px: f32, max_width: i32, scale: i32, color: [u8; 3]) -> Option<TextImage> {
        let font = self.font.as_ref()?;
        let sf = font.as_scaled(PxScale::from(px * scale as f32));
        let max_w = (max_width * scale) as f32;

        let width_of = |s: &str| {
            let mut w = 0.0;
            let mut prev = None;
            for c in s.chars() {
                let id = sf.glyph_id(c);
                if let Some(p) = prev {
                    w += sf.kern(p, id);
                }
                w += sf.h_advance(id);
                prev = Some(id);
            }
            w
        };
        let mut line = text.to_string();
        if width_of(&line) > max_w {
            let ell = width_of("…");
            let mut chars: Vec<char> = text.chars().collect();
            while !chars.is_empty() && width_of(&chars.iter().collect::<String>()) + ell > max_w {
                chars.pop();
            }
            line = chars.into_iter().collect::<String>().trim_end().to_string() + "…";
        }

        let width = (width_of(&line).ceil() as i32).max(1);
        let height = ((sf.ascent() - sf.descent()).ceil() as i32).max(1);
        let mut coverage = vec![0f32; (width * height) as usize];
        let mut caret = 0.0;
        let mut prev = None;
        for c in line.chars() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                caret += sf.kern(p, id);
            }
            let glyph = id.with_scale_and_position(sf.scale(), point(caret, sf.ascent()));
            caret += sf.h_advance(id);
            prev = Some(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                outline.draw(|x, y, c| {
                    let (px, py) = (b.min.x as i32 + x as i32, b.min.y as i32 + y as i32);
                    if (0..width).contains(&px) && (0..height).contains(&py) {
                        let i = (py * width + px) as usize;
                        coverage[i] = coverage[i].max(c);
                    }
                });
            }
        }

        let mut pixels = Vec::with_capacity(coverage.len() * 4);
        for a in coverage {
            let a = a.clamp(0.0, 1.0);
            let pm = |v: u8| (v as f32 * a).round() as u8;
            pixels.extend_from_slice(&[pm(color[2]), pm(color[1]), pm(color[0]), (a * 255.0).round() as u8]);
        }
        Some(TextImage { pixels, width, height })
    }
}

fn find_font() -> Option<PathBuf> {
    if let Ok(out) = Command::new("fc-match").args(["-f", "%{file}", "sans-serif"]).output() {
        let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if out.status.success() && path.is_file() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = [
        format!("{home}/.local/share/fonts"),
        format!("{home}/.nix-profile/share/fonts"),
        "/run/current-system/sw/share/X11/fonts".into(),
        "/run/current-system/sw/share/fonts".into(),
        "/usr/share/fonts".into(),
    ];
    let wanted = ["DejaVuSans.ttf", "NotoSans-Regular.ttf", "Cantarell-Regular.otf", "LiberationSans-Regular.ttf"];
    dirs.iter().find_map(|d| search(PathBuf::from(d), &wanted, 4))
}

fn search(dir: PathBuf, wanted: &[&str], depth: u32) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(&dir).ok()?.flatten().collect();
    for e in &entries {
        if wanted.iter().any(|w| e.file_name() == *w) {
            return Some(e.path());
        }
    }
    if depth == 0 {
        return None;
    }
    entries.iter().filter(|e| e.path().is_dir()).find_map(|e| search(e.path(), wanted, depth - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_truncates() {
        let t = TextRenderer::new();
        if t.font.is_none() {
            return; // no fonts in this environment
        }
        let img = t.render("Hello", 14.0, 400, 1, [255, 255, 255]).unwrap();
        assert!(img.width > 10 && img.height > 5);
        assert_eq!(img.pixels.len(), (img.width * img.height * 4) as usize);
        assert!(img.pixels.chunks(4).any(|p| p[3] > 200), "some ink");

        let long = "a very long window title that cannot possibly fit in the space given";
        let img = t.render(long, 14.0, 100, 1, [255, 255, 255]).unwrap();
        assert!(img.width <= 100);
    }
}
