//! Freedesktop icon themes, for compositor UI (the title bar's close button).
//!
//! Lookup follows the icon theme spec, simplified: the theme's `index.theme` lists its
//! directories (with sizes); the best match for the wanted size wins, then the themes
//! it `Inherits`, then `hicolor`. SVGs are rendered with resvg, PNGs decoded directly.
//! Symbolic icons (`*-symbolic`) are recoloured, as GTK does.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

/// A rendered icon: premultiplied ARGB8888 (little-endian B, G, R, A), square.
#[derive(Debug, Clone)]
pub struct IconImage {
    pub pixels: Vec<u8>,
    pub size: i32,
}

/// Directories that may contain icon themes, in priority order.
fn icon_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(data_home) =
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")))
    {
        dirs.push(data_home.join("icons"));
    }
    if let Some(h) = &home {
        dirs.push(h.join(".icons"));
    }
    if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
        dirs.extend(std::env::split_paths(&data_dirs).map(|d| d.join("icons")));
    }
    // Usual places, in case XDG_DATA_DIRS was trimmed (e.g. inside a dev shell).
    if let Some(h) = &home {
        dirs.push(h.join(".nix-profile/share/icons"));
    }
    if let Some(user) = std::env::var_os("USER") {
        dirs.push(Path::new("/etc/profiles/per-user").join(user).join("share/icons"));
    }
    dirs.push("/run/current-system/sw/share/icons".into());
    dirs.push("/usr/local/share/icons".into());
    dirs.push("/usr/share/icons".into());
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()) && d.is_dir());
    dirs
}

/// One `[directory]` section of an index.theme.
#[derive(Debug)]
struct ThemeDir {
    path: String,
    size: i32,
    scalable: bool,
}

#[derive(Debug, Default)]
struct ThemeIndex {
    dirs: Vec<ThemeDir>,
    inherits: Vec<String>,
}

fn parse_index(text: &str) -> ThemeIndex {
    let mut index = ThemeIndex::default();
    let mut listed: Vec<String> = Vec::new();
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if current == "Icon Theme" {
            match k {
                "Directories" | "ScaledDirectories" => listed.extend(v.split(',').map(|s| s.trim().to_string())),
                "Inherits" => index.inherits = v.split(',').map(|s| s.trim().to_string()).collect(),
                _ => {}
            }
        } else {
            sections.entry(current.clone()).or_default().insert(k.to_string(), v.to_string());
        }
    }
    for path in listed.into_iter().filter(|p| !p.is_empty()) {
        let section = sections.get(&path);
        let get = |k: &str| section.and_then(|s| s.get(k));
        let size = get("Size").and_then(|s| s.parse().ok()).unwrap_or(16);
        let scalable = get("Type").is_some_and(|t| t == "Scalable");
        index.dirs.push(ThemeDir { path, size, scalable });
    }
    index
}

/// Find `name` (no extension) in `theme` and the themes it inherits, for `size` px.
pub fn find_icon(theme: &str, name: &str, size: i32) -> Option<PathBuf> {
    let bases = icon_dirs();
    let mut queue: Vec<String> = vec![theme.to_string()];
    let mut visited = HashSet::new();
    while let Some(theme) = queue.first().cloned() {
        queue.remove(0);
        if !visited.insert(theme.clone()) {
            continue;
        }
        for base in &bases {
            let root = base.join(&theme);
            let Ok(text) = std::fs::read_to_string(root.join("index.theme")) else { continue };
            let index = parse_index(&text);
            // Scalable first (they fit any size), then the closest fixed size.
            let mut dirs: Vec<&ThemeDir> = index.dirs.iter().collect();
            dirs.sort_by_key(|d| if d.scalable { 0 } else { 1 + (d.size - size).abs() });
            for dir in dirs {
                for ext in ["svg", "png"] {
                    let path = root.join(&dir.path).join(format!("{name}.{ext}"));
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
            queue.extend(index.inherits.iter().cloned());
        }
    }
    if theme != "hicolor" && !visited.contains("hicolor") {
        return find_icon("hicolor", name, size);
    }
    None
}

/// Render an icon file to `size` x `size` px. Symbolic icons are drawn in `color`.
pub fn render_icon(path: &Path, size: i32, color: [u8; 3]) -> Option<IconImage> {
    let size = size.max(1);
    let rgba_premul: Vec<u8> = match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => {
            use resvg::{tiny_skia, usvg};
            let data = std::fs::read(path).ok()?;
            let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
            let mut pixmap = tiny_skia::Pixmap::new(size as u32, size as u32)?;
            let s = tree.size();
            let transform = tiny_skia::Transform::from_scale(size as f32 / s.width(), size as f32 / s.height());
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            pixmap.take()
        }
        Some("png") => {
            // Scaled to fit by the renderer later; decode as-is, then nearest-resample.
            let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
            let mut reader = decoder.read_info().ok()?;
            let mut buf = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf).ok()?;
            if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
                return None;
            }
            let (w, h) = (info.width as usize, info.height as usize);
            let mut out = Vec::with_capacity((size * size * 4) as usize);
            for y in 0..size as usize {
                for x in 0..size as usize {
                    let (sx, sy) = (x * w / size as usize, y * h / size as usize);
                    let p = &buf[(sy * w + sx) * 4..][..4];
                    let a = p[3] as u32;
                    out.extend_from_slice(&[
                        (p[0] as u32 * a / 255) as u8,
                        (p[1] as u32 * a / 255) as u8,
                        (p[2] as u32 * a / 255) as u8,
                        p[3],
                    ]);
                }
            }
            out
        }
        _ => return None,
    };

    let symbolic = path.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.ends_with("-symbolic"));
    let mut pixels = Vec::with_capacity(rgba_premul.len());
    for p in rgba_premul.as_chunks::<4>().0 {
        let a = p[3];
        let (r, g, b) = if symbolic {
            // Keep the shape (alpha), paint it in `color`.
            let pm = |c: u8| (c as u32 * a as u32 / 255) as u8;
            (pm(color[0]), pm(color[1]), pm(color[2]))
        } else {
            (p[0], p[1], p[2])
        };
        pixels.extend_from_slice(&[b, g, r, a]);
    }
    Some(IconImage { pixels, size })
}

/// (theme, names, size, colour).
type CacheKey = (String, String, i32, [u8; 3]);

thread_local! {
    /// (theme, name, size, colour) -> rendered icon (or None if not found).
    static CACHE: RefCell<HashMap<CacheKey, Option<IconImage>>> = RefCell::new(HashMap::new());
}

/// Look up and render an icon, trying `names` in order; cached.
pub fn icon(theme: &str, names: &[&str], size: i32, color: [u8; 3]) -> Option<IconImage> {
    let key = (theme.to_string(), names.join(","), size, color);
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let found = names.iter().find_map(|n| find_icon(theme, n, size));
                match &found {
                    Some(path) => tracing::debug!(?path, "icon"),
                    None => tracing::warn!(theme, ?names, "icon not found in the theme (or hicolor)"),
                }
                found.and_then(|p| render_icon(&p, size, color))
            })
            .clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_index_theme() {
        let index = parse_index(
            "[Icon Theme]\nName=Test\nInherits=hicolor\nDirectories=16x16/ui,symbolic/ui\n\n\
             [16x16/ui]\nSize=16\nType=Fixed\n\n[symbolic/ui]\nSize=16\nType=Scalable\n",
        );
        assert_eq!(index.inherits, vec!["hicolor"]);
        assert_eq!(index.dirs.len(), 2);
        assert!(!index.dirs[0].scalable && index.dirs[1].scalable);
    }

    #[test]
    fn finds_and_renders_adwaita_close_if_installed() {
        let Some(path) = find_icon("Adwaita", "window-close-symbolic", 16) else {
            return; // not installed here
        };
        let img = render_icon(&path, 16, [255, 255, 255]).unwrap();
        assert_eq!(img.pixels.len(), 16 * 16 * 4);
        assert!(img.pixels.chunks(4).any(|p| p[3] > 128), "the icon has a shape");
        // Recoloured: every visible pixel is white (premultiplied).
        assert!(img.pixels.chunks(4).all(|p| p[0] == p[3] && p[1] == p[3] && p[2] == p[3]));
    }
}
