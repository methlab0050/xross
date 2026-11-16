//! Cursor drawing for backends without a host cursor (udev): xcursor themes for named
//! cursors, plus client-provided cursor surfaces.

use std::{collections::HashMap, io::Read, sync::Mutex, time::Duration};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
        },
    },
    input::pointer::{CursorIcon, CursorImageAttributes, CursorImageStatus},
    utils::{IsAlive, Logical, Physical, Point, Scale, Transform},
    wayland::compositor::with_states,
};
use xcursor::{
    CursorTheme,
    parser::{Image, parse_xcursor},
};

use crate::render::OutputRenderElement;

static FALLBACK_CURSOR_DATA: &[u8] = include_bytes!("../resources/cursor.rgba");

pub struct CursorManager {
    theme: CursorTheme,
    size: u32,
    /// Parsed xcursor frames per icon name.
    icons: HashMap<&'static str, Vec<Image>>,
    /// Uploaded buffers per (icon, frame index), so textures are reused across frames.
    buffers: HashMap<(&'static str, usize), MemoryRenderBuffer>,
}

impl CursorManager {
    pub fn new() -> Self {
        if std::env::var_os("XCURSOR_PATH").is_none() {
            // TTY sessions may not have it; include NixOS's system icon dir.
            let home = std::env::var("HOME").unwrap_or_default();
            let path =
                format!("{home}/.local/share/icons:{home}/.icons:/run/current-system/sw/share/icons:/usr/share/icons");
            unsafe { std::env::set_var("XCURSOR_PATH", path) };
        }
        let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Adwaita".into());
        let size = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
        Self { theme: CursorTheme::load(&name), size, icons: HashMap::new(), buffers: HashMap::new() }
    }

    fn images(&mut self, icon: CursorIcon) -> &[Image] {
        let name = icon.name();
        if !self.icons.contains_key(name) {
            let images = load_icon(&self.theme, name)
                .or_else(|| icon.alt_names().iter().find_map(|alt| load_icon(&self.theme, alt)))
                .or_else(|| load_icon(&self.theme, "default"))
                .unwrap_or_else(|| {
                    vec![Image {
                        size: 32,
                        width: 64,
                        height: 64,
                        xhot: 1,
                        yhot: 1,
                        delay: 1,
                        pixels_rgba: Vec::from(FALLBACK_CURSOR_DATA),
                        pixels_argb: vec![],
                    }]
                });
            self.icons.insert(name, images);
        }
        &self.icons[name]
    }

    /// Render elements for the cursor at `pos` (output-local logical coordinates).
    pub fn render_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        status: &CursorImageStatus,
        pos: Point<f64, Logical>,
        scale: Scale<f64>,
        time: Duration,
    ) -> Vec<OutputRenderElement> {
        let fallback = CursorImageStatus::default_named();
        let status = match status {
            CursorImageStatus::Surface(s) if !s.alive() => &fallback,
            s => s,
        };
        match status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<Mutex<CursorImageAttributes>>()
                        .map(|a| a.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let loc: Point<i32, Physical> = (pos - hotspot.to_f64()).to_physical(scale).to_i32_round();
                render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    surface,
                    loc,
                    scale,
                    1.0,
                    Kind::Cursor,
                )
                .into_iter()
                .map(OutputRenderElement::Surface)
                .collect()
            }
            CursorImageStatus::Named(icon) => {
                let icon = *icon;
                let size = self.size * scale.x.ceil().max(1.0) as u32;
                let (idx, image) = frame(time, size, self.images(icon));
                let (xhot, yhot, buffer_scale) = (image.xhot, image.yhot, (image.size / self.size).max(1));
                let buffer = self
                    .buffers
                    .entry((icon.name(), idx))
                    .or_insert_with(|| {
                        MemoryRenderBuffer::from_slice(
                            &image.pixels_rgba,
                            Fourcc::Abgr8888,
                            (image.width as i32, image.height as i32),
                            buffer_scale as i32,
                            Transform::Normal,
                            None,
                        )
                    })
                    .clone();
                let hotspot =
                    Point::<f64, Logical>::from((xhot as f64 / buffer_scale as f64, yhot as f64 / buffer_scale as f64));
                let loc = (pos - hotspot).to_physical(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc,
                    &buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(e) => vec![OutputRenderElement::Memory(e)],
                    Err(err) => {
                        tracing::warn!(?err, "failed to upload cursor");
                        Vec::new()
                    }
                }
            }
        }
    }
}

fn load_icon(theme: &CursorTheme, name: &str) -> Option<Vec<Image>> {
    let path = theme.load_icon(name)?;
    let mut data = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut data).ok()?;
    parse_xcursor(&data).filter(|images| !images.is_empty())
}

/// Pick the animation frame (and its index) for `time` among images nearest to `size`.
fn frame(time: Duration, size: u32, images: &[Image]) -> (usize, Image) {
    let nearest = images.iter().min_by_key(|i| (size as i32 - i.size as i32).abs()).expect("cursor without images");
    let frames: Vec<(usize, &Image)> =
        images.iter().enumerate().filter(|(_, i)| i.width == nearest.width && i.height == nearest.height).collect();
    let total: u32 = frames.iter().map(|(_, i)| i.delay).sum();
    if total == 0 {
        return (frames[0].0, frames[0].1.clone());
    }
    let mut millis = time.as_millis() as u32 % total;
    for (idx, img) in &frames {
        if millis < img.delay {
            return (*idx, (*img).clone());
        }
        millis -= img.delay;
    }
    (frames[0].0, frames[0].1.clone())
}
