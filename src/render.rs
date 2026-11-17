//! Builds the per-frame render element list for an output, front-to-back:
//! cursor, switcher, workspace OSD, a window being dragged in the overview,
//! overlay/top layers, workspaces (windows with title bars, plus overview tiles and
//! slots), bottom/background layers.
//!
//! Workspaces are drawn through [`XrossState::workspace_views`], which covers the
//! normal view, sliding between workspaces, and the overview alike.

use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Kind,
            memory::MemoryRenderBufferRenderElement,
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            utils::{CropRenderElement, RescaleRenderElement},
        },
        gles::GlesRenderer,
    },
    desktop::{Window, layer_map_for_output},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
    wayland::shell::wlr_layer::Layer,
};

use crate::{
    layout::grid::Coord,
    shell::{
        PointerHit, has_ssd,
        snap::{OPEN_SCALE, open_progress, tick_window_animations},
        ssd::{self, DecorationStyle},
        workspace::{WsId, workspace_of},
    },
    state::XrossState,
};

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    /// Window surfaces: scaled for animations and the overview, cropped to their workspace.
    Window=CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    Solid=SolidColorRenderElement,
    /// Title bars, scaled and cropped like their windows.
    CroppedSolid=CropRenderElement<RescaleRenderElement<SolidColorRenderElement>>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

// Workspace minimap OSD.
const OSD_CELL: (i32, i32) = (36, 22);
const OSD_GAP: i32 = 6;
const OSD_PAD: i32 = 12;
const OSD_BG: [f32; 4] = [0.05, 0.05, 0.07, 0.88];
const OSD_HOLE: [f32; 4] = [0.11, 0.11, 0.13, 1.0];
const OSD_EMPTY: [f32; 4] = [0.20, 0.20, 0.23, 1.0];
const OSD_ACTIVE: [f32; 4] = [0.50, 0.52, 0.57, 1.0];
const OSD_CURRENT: [f32; 4] = [0.36, 0.56, 0.96, 1.0];

// Overview.
const TILE_BG: [f32; 4] = [0.12, 0.12, 0.15, 1.0];
const SLOT_BG: [f32; 4] = [0.10, 0.10, 0.12, 1.0];
const SLOT_BORDER: [f32; 4] = [0.22, 0.22, 0.26, 1.0];
const RING: [f32; 4] = [0.36, 0.56, 0.96, 1.0];
const RING_WIDTH: f64 = 3.0;
const DRAG_ALPHA: f32 = 0.85;

/// Buffers for the workspace minimap, reused across frames so damage stays minimal.
#[derive(Default)]
pub struct OsdBuffers {
    bg: SolidColorBuffer,
    cells: Vec<SolidColorBuffer>,
}

/// Buffers for overview tiles and slots, keyed so their ids stay stable.
#[derive(Default)]
pub struct OverviewBuffers {
    tiles: HashMap<WsId, SolidColorBuffer>,
    slots: HashMap<Coord, (SolidColorBuffer, SolidColorBuffer)>,
    ring: SolidColorBuffer,
}

fn solid(
    buf: &mut SolidColorBuffer,
    rect: Rectangle<f64, Logical>,
    color: [f32; 4],
    alpha: f32,
    scale: Scale<f64>,
) -> SolidColorRenderElement {
    let r = rect.to_i32_round::<i32>();
    buf.update(r.size, color);
    SolidColorRenderElement::from_buffer(buf, r.loc.to_physical_precise_round(scale), scale, alpha, Kind::Unspecified)
}

impl XrossState {
    /// Draw `window` with its content filling `target` (output-local), title bar scaled
    /// by `deco_scale`, everything cropped to `crop`.
    #[allow(clippy::too_many_arguments)]
    fn push_window(
        &self,
        renderer: &mut GlesRenderer,
        elements: &mut Vec<OutputRenderElement>,
        window: &Window,
        target: Rectangle<f64, Logical>,
        deco_scale: f64,
        crop: Rectangle<i32, Physical>,
        alpha: f32,
        style: Option<DecorationStyle>,
        scale: Scale<f64>,
    ) {
        let geo = window.geometry();
        let stretch =
            Scale { x: target.size.w / geo.size.w.max(1) as f64, y: target.size.h / geo.size.h.max(1) as f64 };
        let origin: Point<i32, Physical> = target.loc.to_physical_precise_round(scale);
        let surface_loc = origin - geo.loc.to_physical_precise_round(scale);
        elements.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, surface_loc, scale, alpha)
                .into_iter()
                .filter_map(|e| {
                    let e = RescaleRenderElement::from_element(e, origin, stretch);
                    CropRenderElement::from_element(e, scale, crop).map(OutputRenderElement::Window)
                }),
        );

        if let Some(style) = style.filter(|_| has_ssd(window)) {
            // Lay the title bar out at full size anchored at the content origin, then
            // scale it around that origin like the content.
            let unscaled = Rectangle::new(target.loc, target.size.downscale(deco_scale)).to_i32_round::<i32>();
            elements.extend(
                ssd::render_elements(window, unscaled, Point::default(), scale, style, alpha)
                    .into_iter()
                    .filter_map(|e| {
                        let e = RescaleRenderElement::from_element(e, origin, deco_scale);
                        CropRenderElement::from_element(e, scale, crop)
                    })
                    .map(OutputRenderElement::CroppedSolid),
            );
        }
    }

    pub fn output_elements(&self, renderer: &mut GlesRenderer, output: &Output) -> Vec<OutputRenderElement> {
        self.output_elements_for(renderer, output, self.draw_cursor)
    }

    /// Like [`Self::output_elements`], choosing whether to draw the cursor (screen
    /// captures ask for it either way).
    pub fn output_elements_for(
        &self,
        renderer: &mut GlesRenderer,
        output: &Output,
        draw_cursor: bool,
    ) -> Vec<OutputRenderElement> {
        let Some(output_geo) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = Scale::from(output.current_scale().fractional_scale());
        let now = self.start_time.elapsed();
        let p = self.overview_progress(now);
        let in_overview = p > 0.0 || self.overview_active();
        let mut elements = Vec::new();

        let pointer_pos = self.seat.get_pointer().unwrap().current_location();
        if draw_cursor && output_geo.to_f64().contains(pointer_pos) {
            elements.extend(self.cursor.borrow_mut().render_elements(
                renderer,
                &self.cursor_status,
                pointer_pos - output_geo.loc.to_f64(),
                scale,
                now,
            ));
        }

        // Locked (or locking): the lock surface on black, and nothing else.
        if self.is_locked() {
            if let Some(surface) = self.lock_surface() {
                elements.extend(
                    render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<GlesRenderer>>(
                        renderer,
                        surface,
                        Point::<i32, Physical>::default(),
                        scale,
                        1.0,
                        Kind::Unspecified,
                    )
                    .into_iter()
                    .map(OutputRenderElement::Surface),
                );
            }
            return elements;
        }

        elements.extend(self.switcher_elements(renderer, scale));

        if self.workspace_osd_visible() && !in_overview {
            elements.extend(self.workspace_osd_elements(output_geo, scale).into_iter().map(OutputRenderElement::Solid));
        }

        let output_crop = Rectangle::from_size(output_geo.size.to_physical_precise_round(scale));
        if let Some((window, target, s)) = self.dragged_window(now) {
            self.push_window(renderer, &mut elements, &window, target, s, output_crop, DRAG_ALPHA, None, scale);
        }

        // Must run before locking the layer map below (pointer_hit locks it too).
        let focused = self.focused_window();
        let hovered_close = self.pointer_hit(pointer_pos).and_then(|h| match h {
            PointerHit::CloseButton(w) => Some(w),
            _ => None,
        });

        let layers = layer_map_for_output(output);
        let push_layers =
            |renderer: &mut GlesRenderer, elements: &mut Vec<OutputRenderElement>, kind: Layer, alpha: f32| {
                if alpha <= 0.0 {
                    return;
                }
                // layers_on() is bottom-to-top within a layer; we need front-to-back.
                for layer in layers.layers_on(kind).rev() {
                    let Some(geo) = layers.layer_geometry(layer) else {
                        continue;
                    };
                    let loc = (geo.loc - layer.geometry().loc).to_physical_precise_round(scale);
                    elements.extend(
                        layer
                            .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, alpha)
                            .into_iter()
                            .map(OutputRenderElement::Surface),
                    );
                }
            };

        // Panels fade out as the overview opens. A fullscreen window on top covers them
        // too (but not overlays like notifications).
        let panel_alpha = (1.0 - p.clamp(0.0, 1.0)) as f32;
        let fullscreen_on_top = !in_overview && self.fullscreen_on_top();
        push_layers(renderer, &mut elements, Layer::Overlay, 1.0);
        if !fullscreen_on_top {
            push_layers(renderer, &mut elements, Layer::Top, panel_alpha);
        }

        let views = self.workspace_views(now);
        let full = Rectangle::from_size(output_geo.size.to_f64());
        for window in self.space.elements().rev() {
            if self.is_dragged(window) {
                continue;
            }
            // Windows on workspaces out of view are skipped; untagged ones (X11 menus)
            // stick to the screen, but not in the overview.
            let (tile, s) = match workspace_of(window) {
                None if in_overview => continue,
                None => (full, 1.0),
                Some(ws) => match views.iter().find(|v| v.id == Some(ws)) {
                    Some(v) => (v.rect, v.scale),
                    None => continue,
                },
            };
            let Some(mut rect) = self.rendered_rect(window, now) else {
                continue;
            };
            let mut alpha = 1.0f32;
            if let Some(op) = open_progress(window, now) {
                alpha = op.clamp(0.0, 1.0) as f32;
                let k = OPEN_SCALE + (1.0 - OPEN_SCALE) * op;
                let center = rect.loc + rect.size.to_point().downscale(2.0);
                rect.size = rect.size.upscale(k);
                rect.loc = center - rect.size.to_point().downscale(2.0);
            }
            // Into the tile: output-local, scaled.
            let target =
                Rectangle::new(tile.loc + (rect.loc - output_geo.loc.to_f64()).upscale(s), rect.size.upscale(s));
            let title = ssd::TITLE_HEIGHT as f64 * s;
            let with_title =
                Rectangle::new(target.loc - Point::from((0.0, title)), target.size + Size::from((0.0, title)));
            if !full.overlaps(with_title) {
                continue;
            }
            // Crop to the window's own workspace so nothing spills into a neighbour.
            let crop = tile.to_physical_precise_round(scale);
            let style = DecorationStyle {
                focused: focused.as_ref() == Some(window),
                close_hovered: hovered_close.as_ref() == Some(window),
            };
            self.push_window(renderer, &mut elements, window, target, s, crop, alpha, Some(style), scale);
        }

        if in_overview {
            elements.extend(self.overview_backdrop(&views, p.clamp(0.0, 1.0) as f32, scale));
        }

        if fullscreen_on_top {
            push_layers(renderer, &mut elements, Layer::Top, panel_alpha);
        }
        push_layers(renderer, &mut elements, Layer::Bottom, panel_alpha);
        push_layers(renderer, &mut elements, Layer::Background, 1.0);
        elements
    }

    /// Overview tile backgrounds, drop slots and the selection ring (front to back).
    fn overview_backdrop(
        &self,
        views: &[crate::shell::overview::WsView],
        alpha: f32,
        scale: Scale<f64>,
    ) -> Vec<OutputRenderElement> {
        let now = self.start_time.elapsed();
        let mut ui = self.overview_ui.borrow_mut();
        let ui = &mut *ui;
        let mut out = Vec::new();
        for v in views {
            let Some(id) = v.id else { continue };
            let buf = ui.tiles.entry(id).or_default();
            out.push(OutputRenderElement::Solid(solid(buf, v.rect, TILE_BG, alpha, scale)));
        }
        ui.tiles.retain(|id, _| views.iter().any(|v| v.id == Some(*id)));

        let slots = self.slot_views(now);
        for s in &slots {
            let (fill, border) = ui.slots.entry(s.coord).or_default();
            let inner = Rectangle::new(s.rect.loc + Point::from((2.0, 2.0)), s.rect.size - Size::from((4.0, 4.0)));
            out.push(OutputRenderElement::Solid(solid(fill, inner, SLOT_BG, alpha, scale)));
            out.push(OutputRenderElement::Solid(solid(border, s.rect, SLOT_BORDER, alpha, scale)));
        }
        ui.slots.retain(|c, _| slots.iter().any(|s| s.coord == *c));

        if let Some(r) = self.selection_rect(now) {
            let ring = Rectangle::new(
                r.loc - Point::from((RING_WIDTH, RING_WIDTH)),
                r.size + Size::from((2.0 * RING_WIDTH, 2.0 * RING_WIDTH)),
            );
            out.push(OutputRenderElement::Solid(solid(&mut ui.ring, ring, RING, alpha, scale)));
        }
        out
    }
}

impl XrossState {
    /// The workspace grid as a small map in the middle of the output: holes in the grid
    /// are dark, empty workspaces dim, active ones grey, the current one blue.
    fn workspace_osd_elements(
        &self,
        output_geo: Rectangle<i32, Logical>,
        scale: Scale<f64>,
    ) -> Vec<SolidColorRenderElement> {
        let (cells, (x0, y0, x1, y1)) = self.osd_cells();
        let (cols, rows) = (x1 - x0 + 1, y1 - y0 + 1);
        let size = Size::<i32, Logical>::from((
            cols * OSD_CELL.0 + (cols - 1) * OSD_GAP + 2 * OSD_PAD,
            rows * OSD_CELL.1 + (rows - 1) * OSD_GAP + 2 * OSD_PAD,
        ));
        // Relative to the output.
        let origin = Point::<i32, Logical>::from(((output_geo.size.w - size.w) / 2, (output_geo.size.h - size.h) / 2));
        let phys = |p: Point<i32, Logical>| -> Point<i32, Physical> { p.to_physical_precise_round(scale) };

        let mut bufs = self.ws_osd.borrow_mut();
        let bufs = &mut *bufs;
        bufs.cells.resize_with((cols * rows) as usize, SolidColorBuffer::default);
        let mut out = Vec::with_capacity(bufs.cells.len() + 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let color = match cells.iter().find(|(c, ..)| *c == (x, y)) {
                    Some((_, _, true)) => OSD_CURRENT,
                    Some((_, true, _)) => OSD_ACTIVE,
                    Some(_) => OSD_EMPTY,
                    None => OSD_HOLE,
                };
                let (cx, cy) = (x - x0, y - y0);
                let buf = &mut bufs.cells[(cy * cols + cx) as usize];
                buf.update(OSD_CELL, color);
                let loc = origin
                    + Point::from((OSD_PAD + cx * (OSD_CELL.0 + OSD_GAP), OSD_PAD + cy * (OSD_CELL.1 + OSD_GAP)));
                out.push(SolidColorRenderElement::from_buffer(buf, phys(loc), scale, 1.0, Kind::Unspecified));
            }
        }
        bufs.bg.update(size, OSD_BG);
        out.push(SolidColorRenderElement::from_buffer(&bufs.bg, phys(origin), scale, 1.0, Kind::Unspecified));
        out
    }

    /// Per-frame housekeeping after rendering an output: frame callbacks and cleanup.
    pub fn post_render(&mut self, output: &Output) {
        let area = self.usable_area();
        if area != self.last_usable_area {
            self.last_usable_area = area;
            self.relayout();
        }

        let now = self.start_time.elapsed();
        self.tick_camera();
        self.tick_overview();
        tick_window_animations(self.space.elements(), now);

        // A frame without any windows is on screen now, so a pending lock is safe.
        if self.is_locked() {
            self.confirm_session_lock();
            if let Some(surface) = self.lock_surface() {
                smithay::desktop::utils::send_frames_surface_tree(
                    surface,
                    output,
                    now,
                    Some(std::time::Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }
        }

        // Frames for every window on screen, including neighbours sliding in.
        let throttle = Some(std::time::Duration::ZERO);
        let in_view: Vec<_> = self.workspace_views(now).into_iter().filter_map(|v| v.id).collect();
        for window in self.space.elements() {
            if workspace_of(window).is_none_or(|ws| in_view.contains(&ws)) {
                window.send_frame(output, now, throttle, |_, _| Some(output.clone()));
            }
        }
        for layer in layer_map_for_output(output).layers() {
            smithay::desktop::utils::send_frames_surface_tree(layer.wl_surface(), output, now, throttle, |_, _| {
                Some(output.clone())
            });
        }
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.cursor_status {
            smithay::desktop::utils::send_frames_surface_tree(surface, output, now, throttle, |_, _| {
                Some(output.clone())
            });
        }

        self.space.refresh();
        self.popups.cleanup();
        layer_map_for_output(output).cleanup();
        let _ = self.display_handle.flush_clients();
    }
}

impl XrossState {
    /// Presentation-time feedback for everything drawn on `output` this frame; the
    /// backend marks it presented at vblank (or discards it if nothing was shown).
    pub fn presentation_feedback(&self, output: &Output) -> smithay::desktop::utils::OutputPresentationFeedback {
        use smithay::{
            desktop::utils::{OutputPresentationFeedback, take_presentation_feedback_surface_tree},
            reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind,
        };
        let mut feedback = OutputPresentationFeedback::new(output);
        let on_output = |_: &_, _: &_| Some(output.clone());
        let no_flags = |_: &_, _: &_| Kind::empty();

        if let Some(surface) = self.lock_surface() {
            take_presentation_feedback_surface_tree(surface, &mut feedback, on_output, no_flags);
            return feedback;
        }
        let now = self.start_time.elapsed();
        let in_view: Vec<_> = self.workspace_views(now).into_iter().filter_map(|v| v.id).collect();
        for window in self.space.elements() {
            if workspace_of(window).is_none_or(|ws| in_view.contains(&ws)) {
                window.take_presentation_feedback(&mut feedback, on_output, no_flags);
            }
        }
        for layer in layer_map_for_output(output).layers() {
            layer.take_presentation_feedback(&mut feedback, on_output, no_flags);
        }
        feedback
    }
}
