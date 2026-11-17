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
    /// Physical size of the image.
    pub width: i32,
    pub height: i32,
    /// Logical size to draw it at (exactly `width / scale` by construction).
    pub logical_width: i32,
    pub logical_height: i32,
}

impl TextRenderer {
    /// Load the font fontconfig picks for `pattern` (a family name like "Noto Sans",
    /// optionally with fontconfig properties: "Noto Sans:bold").
    pub fn for_font(pattern: &str) -> Self {
        let font = find_font(pattern).and_then(|path| {
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

    /// Render `text` at `px` logical pixels for output `scale` (fractional is fine),
    /// truncated with an ellipsis to `max_width` logical pixels.
    ///
    /// The image is rasterised at the exact physical size and padded so it covers a whole
    /// number of logical pixels exactly (`logical * scale` = image size), so drawn at that
    /// logical size on a whole physical pixel, it's never resampled. Glyph baselines and
    /// origins are snapped to pixels for crisp stems.
    pub fn render(&self, text: &str, px: f32, max_width: i32, scale: f64, color: [u8; 3]) -> Option<TextImage> {
        let font = self.font.as_ref()?;
        let s = scale.max(0.1) as f32;
        let sf = font.as_scaled(PxScale::from(px * s));
        let max_w = max_width as f32 * s;

        let width_of = |s: &str| {
            let mut w = 0.0f32;
            let mut prev = None;
            for c in s.chars() {
                let id = sf.glyph_id(c);
                if let Some(p) = prev {
                    w += sf.kern(p, id);
                }
                w = w.round() + sf.h_advance(id);
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

        let ascent = sf.ascent().ceil();
        let ink = (width_of(&line).ceil().max(1.0), (ascent + (-sf.descent()).ceil()).max(1.0));
        let logical_width = (ink.0 / s).ceil() as i32;
        let logical_height = (ink.1 / s).ceil() as i32;
        let width = ((logical_width as f64 * scale).round() as i32).max(1);
        let height = ((logical_height as f64 * scale).round() as i32).max(1);

        let mut coverage = vec![0f32; (width * height) as usize];
        let mut caret = 0.0f32;
        let mut prev = None;
        for c in line.chars() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                caret += sf.kern(p, id);
            }
            caret = caret.round();
            let glyph = id.with_scale_and_position(sf.scale(), point(caret, ascent));
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
            // A little extra contrast on edge pixels: light text on dark bars otherwise
            // looks thin and soft without hinting.
            let a = a.clamp(0.0, 1.0).powf(0.8);
            let pm = |v: u8| (v as f32 * a).round() as u8;
            pixels.extend_from_slice(&[pm(color[2]), pm(color[1]), pm(color[0]), (a * 255.0).round() as u8]);
        }
        Some(TextImage { pixels, width, height, logical_width, logical_height })
    }
}

thread_local! {
    static BY_FONT: std::cell::RefCell<std::collections::HashMap<String, std::rc::Rc<TextRenderer>>> =
        Default::default();
}

/// Use the text renderer for `font` (loaded once per font name).
pub fn with_font<T>(font: &str, f: impl FnOnce(&TextRenderer) -> T) -> T {
    let renderer = BY_FONT.with(|m| {
        m.borrow_mut().entry(font.to_string()).or_insert_with(|| std::rc::Rc::new(TextRenderer::for_font(font))).clone()
    });
    f(&renderer)
}

/// The UI font from the config (`appearance.font`).
pub fn with_ui_font<T>(f: impl FnOnce(&TextRenderer) -> T) -> T {
    let font = crate::config::current().appearance.font.clone();
    with_font(&font, f)
}

fn find_font(pattern: &str) -> Option<PathBuf> {
    let out = Command::new("fc-match").args(["-f", "%{family}\n%{file}", pattern]).output();
    if let Ok(out) = out
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        let (family, file) = text.split_once('\n').unwrap_or(("", &text));
        let path = PathBuf::from(file.trim());
        if path.is_file() {
            // fontconfig always returns *something*; say so when it isn't what was asked.
            let wanted = pattern.split(':').next().unwrap_or(pattern).trim().to_lowercase();
            let generic = matches!(wanted.as_str(), "sans-serif" | "sans" | "serif" | "monospace");
            if !generic && !family.to_lowercase().split(',').any(|f| f.trim() == wanted) {
                tracing::warn!(requested = pattern, using = family, "font not installed; falling back");
            }
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
        let t = TextRenderer::for_font("sans-serif");
        if t.font.is_none() {
            return; // no fonts in this environment
        }
        let img = t.render("Hello", 14.0, 400, 1.0, [255, 255, 255]).unwrap();
        assert!(img.width > 10 && img.height > 5);
        assert_eq!(img.pixels.len(), (img.width * img.height * 4) as usize);
        assert!(img.pixels.chunks(4).any(|p| p[3] > 200), "some ink");

        let long = "a very long window title that cannot possibly fit in the space given";
        let img = t.render(long, 14.0, 100, 1.0, [255, 255, 255]).unwrap();
        assert!(img.width <= 100);
    }
}
