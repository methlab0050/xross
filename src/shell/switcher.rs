//! Alt-tab glue: MRU tracking, the switcher session ([`crate::layout::switcher`]), and
//! drawing the tiles.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                AsRenderElements, Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::WaylandSurfaceRenderElement,
                utils::{CropRenderElement, RescaleRenderElement},
            },
            gles::GlesRenderer,
        },
    },
    desktop::{Window, WindowSurface},
    utils::{IsAlive, Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Scale, Transform},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use super::workspace::workspace_of;
use crate::{
    layout::switcher::{Layout, Session, layout},
    render::OutputRenderElement,
    state::XrossState,
    ui::text::TextRenderer,
};

const PANEL_BG: [f32; 4] = [0.15, 0.15, 0.18, 0.96];
const TILE_SELECTED: [f32; 4] = [0.22, 0.30, 0.48, 1.0];
const TILE_BORDER: [f32; 4] = [0.36, 0.56, 0.96, 1.0];
const BORDER: i32 = 2;
const TITLE_PX: f32 = 13.0;
const TITLE_COLOR: [u8; 3] = [230, 230, 235];

/// Buffers for drawing the switcher, kept across frames so damage stays small.
#[derive(Default)]
pub struct SwitcherUi {
    text: Option<TextRenderer>,
    /// (title, scale) -> rendered text and its logical width.
    titles: HashMap<(String, i32), (MemoryRenderBuffer, i32)>,
    panel: SolidColorBuffer,
    selected: SolidColorBuffer,
    border: SolidColorBuffer,
}

pub fn window_title(window: &Window) -> String {
    let title = match window.underlying_surface() {
        WindowSurface::Wayland(t) => with_states(t.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>()?.lock().ok()?;
            data.title.clone().or_else(|| data.app_id.clone())
        }),
        WindowSurface::X11(x) => Some(x.title()).filter(|t| !t.is_empty()).or_else(|| Some(x.class())),
    };
    title.filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled".into())
}

impl XrossState {
    /// Record that `window` was just focused.
    pub fn touch_mru(&mut self, window: &Window) {
        self.mru.retain(|w| w != window);
        self.mru.insert(0, window.clone());
    }

    /// A window went away: drop it from the MRU list and any running session.
    pub fn forget_mru(&mut self, window: &Window) {
        self.mru.retain(|w| w != window && w.alive());
        if let Some(s) = self.switcher.as_mut()
            && !s.retain(|w| w != window && w.alive())
        {
            self.switcher = None;
        }
    }

    /// The current workspace's windows, most recently used first.
    fn switcher_items(&self) -> Vec<Window> {
        let eligible = |w: &Window| {
            workspace_of(w).is_some_and(|ws| ws == self.current_ws())
                && w.alive()
                && self.space.element_location(w).is_some()
        };
        let mut items: Vec<Window> = self.mru.iter().filter(|w| eligible(w)).cloned().collect();
        // Windows never focused go last, topmost first.
        for w in self.space.elements().rev() {
            if eligible(w) && !items.contains(w) {
                items.push(w.clone());
            }
        }
        items
    }

    /// Mod+Tab (`reverse`: Mod+Shift+Tab).
    pub fn switcher_step(&mut self, reverse: bool) {
        match self.switcher.as_mut() {
            Some(s) if reverse => s.prev(),
            Some(s) => s.next(),
            None => {
                self.switcher = Session::start(self.switcher_items(), reverse, self.start_time.elapsed());
            }
        }
    }

    pub fn switcher_active(&self) -> bool {
        self.switcher.is_some()
    }

    pub fn switcher_visible(&self) -> bool {
        self.switcher.as_ref().is_some_and(|s| s.visible(self.start_time.elapsed()))
    }

    /// Focus the selected window and end the session.
    pub fn switcher_commit(&mut self) {
        let Some(session) = self.switcher.take() else { return };
        let window = session.selected().clone();
        tracing::debug!(title = window_title(&window), "switcher commit");
        if window.alive() {
            self.focus_window(Some(&window), SERIAL_COUNTER.next_serial());
        }
        self.switcher_ui.borrow_mut().titles.clear();
    }

    pub fn switcher_cancel(&mut self) {
        self.switcher = None;
        self.switcher_ui.borrow_mut().titles.clear();
    }

    fn switcher_layout(&self) -> Option<Layout> {
        let session = self.switcher.as_ref()?;
        let sizes: Vec<_> = session.items().iter().map(|w| w.geometry().size).collect();
        Some(layout(&sizes, self.output_rect().size))
    }

    /// The tile under a global pointer position.
    pub fn switcher_tile_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        let local = pos - self.output_rect().loc.to_f64();
        self.switcher_layout()?.tiles.iter().position(|t| t.rect.to_f64().contains(local))
    }

    pub fn switcher_select(&mut self, index: usize) {
        if let Some(s) = self.switcher.as_mut() {
            s.select(index);
        }
    }

    /// Front-to-back elements for the switcher overlay (output-local).
    pub fn switcher_elements(&self, renderer: &mut GlesRenderer, scale: Scale<f64>) -> Vec<OutputRenderElement> {
        if !self.switcher_visible() {
            return Vec::new();
        }
        let (Some(session), Some(layout)) = (self.switcher.as_ref(), self.switcher_layout()) else {
            return Vec::new();
        };
        let phys = |p: Point<i32, Logical>| -> Point<i32, Physical> { p.to_physical_precise_round(scale) };
        let int_scale = scale.x.ceil().max(1.0) as i32;

        let mut ui = self.switcher_ui.borrow_mut();
        let ui = &mut *ui;
        let text = ui.text.get_or_insert_with(TextRenderer::new);
        let mut elements = Vec::new();

        // Titles (front).
        for (window, tile) in session.items().iter().zip(&layout.tiles) {
            let title = window_title(window);
            let key = (title.clone(), int_scale);
            if !ui.titles.contains_key(&key)
                && let Some(img) = text.render(&title, TITLE_PX, tile.title.size.w, int_scale, TITLE_COLOR)
            {
                let buf = MemoryRenderBuffer::from_slice(
                    &img.pixels,
                    Fourcc::Argb8888,
                    (img.width, img.height),
                    int_scale,
                    Transform::Normal,
                    None,
                );
                ui.titles.insert(key.clone(), (buf, img.width / int_scale));
            }
            if let Some((buf, w)) = ui.titles.get(&key) {
                // Centered under the thumbnail.
                let loc = Point::from((tile.title.loc.x + (tile.title.size.w - *w) / 2, tile.title.loc.y));
                if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    phys(loc).to_f64(),
                    buf,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    elements.push(OutputRenderElement::Memory(e));
                }
            }
        }

        // Live thumbnails.
        for (window, tile) in session.items().iter().zip(&layout.tiles) {
            let geo = window.geometry();
            let thumb = tile.thumb.to_f64();
            let stretch =
                Scale { x: thumb.size.w / geo.size.w.max(1) as f64, y: thumb.size.h / geo.size.h.max(1) as f64 };
            let origin = phys(tile.thumb.loc);
            let surface_loc = origin - geo.loc.to_physical_precise_round(scale);
            let crop = Rectangle::new(origin, tile.thumb.size.to_physical_precise_round(scale));
            elements.extend(
                window
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, surface_loc, scale, 1.0)
                    .into_iter()
                    .filter_map(|e| {
                        let e = RescaleRenderElement::from_element(e, origin, stretch);
                        CropRenderElement::from_element(e, scale, crop).map(OutputRenderElement::Window)
                    }),
            );
        }

        // Selection highlight, then the panel.
        if let Some(tile) = layout.tiles.get(session.index()) {
            ui.selected.update(tile.rect.size, TILE_SELECTED);
            let border = Rectangle::new(
                tile.rect.loc - Point::from((BORDER, BORDER)),
                (tile.rect.size.w + 2 * BORDER, tile.rect.size.h + 2 * BORDER).into(),
            );
            ui.border.update(border.size, TILE_BORDER);
            elements.push(OutputRenderElement::Solid(SolidColorRenderElement::from_buffer(
                &ui.selected,
                phys(tile.rect.loc),
                scale,
                1.0,
                Kind::Unspecified,
            )));
            elements.push(OutputRenderElement::Solid(SolidColorRenderElement::from_buffer(
                &ui.border,
                phys(border.loc),
                scale,
                1.0,
                Kind::Unspecified,
            )));
        }
        ui.panel.update(layout.panel.size, PANEL_BG);
        elements.push(OutputRenderElement::Solid(SolidColorRenderElement::from_buffer(
            &ui.panel,
            phys(layout.panel.loc),
            scale,
            1.0,
            Kind::Unspecified,
        )));
        elements
    }
}
