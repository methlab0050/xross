//! Builds the per-frame render element list for an output, front-to-back:
//! cursor, workspace OSD, overlay/top layers, windows (each with its title bar),
//! bottom/background layers.

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Kind,
            memory::MemoryRenderBufferRenderElement,
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
            utils::{CropRenderElement, RescaleRenderElement},
        },
        gles::GlesRenderer,
    },
    desktop::layer_map_for_output,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
    wayland::shell::wlr_layer::Layer,
};

use crate::{
    shell::{
        PointerHit, has_ssd,
        snap::{OPEN_SCALE, open_progress, tick_window_animations},
        ssd::{self, DecorationStyle},
        workspace::workspace_of,
    },
    state::XrossState,
};

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    /// Window surfaces: scaled for animations, cropped to their workspace.
    Window=CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    Solid=SolidColorRenderElement,
    CroppedSolid=CropRenderElement<SolidColorRenderElement>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

pub const CLEAR_COLOR: [f32; 4] = [0.08, 0.08, 0.10, 1.0];

// Workspace minimap OSD.
const OSD_CELL: (i32, i32) = (36, 22);
const OSD_GAP: i32 = 6;
const OSD_PAD: i32 = 12;
const OSD_BG: [f32; 4] = [0.05, 0.05, 0.07, 0.88];
const OSD_HOLE: [f32; 4] = [0.11, 0.11, 0.13, 1.0];
const OSD_EMPTY: [f32; 4] = [0.20, 0.20, 0.23, 1.0];
const OSD_ACTIVE: [f32; 4] = [0.50, 0.52, 0.57, 1.0];
const OSD_CURRENT: [f32; 4] = [0.36, 0.56, 0.96, 1.0];

/// Buffers for the workspace minimap, reused across frames so damage stays minimal.
#[derive(Default)]
pub struct OsdBuffers {
    bg: SolidColorBuffer,
    cells: Vec<SolidColorBuffer>,
}

impl XrossState {
    pub fn output_elements(&self, renderer: &mut GlesRenderer, output: &Output) -> Vec<OutputRenderElement> {
        let Some(output_geo) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = Scale::from(output.current_scale().fractional_scale());
        let mut elements = Vec::new();

        let pointer_pos = self.seat.get_pointer().unwrap().current_location();
        if self.draw_cursor && output_geo.to_f64().contains(pointer_pos) {
            elements.extend(self.cursor.borrow_mut().render_elements(
                renderer,
                &self.cursor_status,
                pointer_pos - output_geo.loc.to_f64(),
                scale,
                self.start_time.elapsed(),
            ));
        }

        elements.extend(self.switcher_elements(renderer, scale));

        if self.workspace_osd_visible() {
            elements.extend(self.workspace_osd_elements(output_geo, scale).into_iter().map(OutputRenderElement::Solid));
        }

        // Must run before locking the layer map below (pointer_hit locks it too).
        let focused = self.focused_window();
        let hovered_close = self.pointer_hit(pointer_pos).and_then(|h| match h {
            PointerHit::CloseButton(w) => Some(w),
            _ => None,
        });

        let layers = layer_map_for_output(output);
        let push_layers = |renderer: &mut GlesRenderer, elements: &mut Vec<OutputRenderElement>, kind: Layer| {
            // layers_on() is bottom-to-top within a layer; we need front-to-back.
            for layer in layers.layers_on(kind).rev() {
                let Some(geo) = layers.layer_geometry(layer) else {
                    continue;
                };
                let loc = (geo.loc - layer.geometry().loc).to_physical_precise_round(scale);
                elements.extend(
                    layer
                        .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, 1.0)
                        .into_iter()
                        .map(OutputRenderElement::Surface),
                );
            }
        };

        // A fullscreen window on top covers panels too (but not overlays like notifications).
        let fullscreen_on_top = self.fullscreen_on_top();
        push_layers(renderer, &mut elements, Layer::Overlay);
        if !fullscreen_on_top {
            push_layers(renderer, &mut elements, Layer::Top);
        }

        let now = self.start_time.elapsed();
        let views = self.workspace_views(now);
        let output_size = output_geo.size.to_physical_precise_round(scale);
        for window in self.space.elements().rev() {
            // Windows on workspaces out of view are skipped; untagged ones (X11 menus) stick.
            let offset = match workspace_of(window) {
                None => Point::default(),
                Some(ws) => match views.iter().find(|(id, _)| *id == ws) {
                    Some((_, offset)) => *offset,
                    None => continue,
                },
            };
            let Some(mut rect) = self.rendered_rect(window, now) else {
                continue;
            };
            let mut alpha = 1.0f32;
            if let Some(p) = open_progress(window, now) {
                alpha = p.clamp(0.0, 1.0) as f32;
                let k = OPEN_SCALE + (1.0 - OPEN_SCALE) * p;
                let center = rect.loc + rect.size.to_point().downscale(2.0);
                rect.size = rect.size.upscale(k);
                rect.loc = center - rect.size.to_point().downscale(2.0);
            }
            // Output-local from here on.
            rect.loc += offset - output_geo.loc.to_f64();
            let content = rect.to_i32_round::<i32>();
            let local_output = Rectangle::from_size(output_geo.size);
            if !local_output.overlaps(content) && !local_output.overlaps(ssd::title_rect(content)) {
                continue;
            }

            // Crop to the window's own workspace so nothing spills into a neighbour mid-slide.
            let crop = Rectangle::new(offset.to_physical_precise_round(scale), output_size);
            let geo = window.geometry();
            let stretch =
                Scale { x: rect.size.w / geo.size.w.max(1) as f64, y: rect.size.h / geo.size.h.max(1) as f64 };
            let origin: Point<i32, Physical> = rect.loc.to_physical_precise_round(scale);
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

            if has_ssd(window) {
                let style = DecorationStyle {
                    focused: focused.as_ref() == Some(window),
                    close_hovered: hovered_close.as_ref() == Some(window),
                };
                elements.extend(
                    ssd::render_elements(window, content, Point::default(), scale, style, alpha)
                        .into_iter()
                        .filter_map(|e| CropRenderElement::from_element(e, scale, crop))
                        .map(OutputRenderElement::CroppedSolid),
                );
            }
        }

        if fullscreen_on_top {
            push_layers(renderer, &mut elements, Layer::Top);
        }
        push_layers(renderer, &mut elements, Layer::Bottom);
        push_layers(renderer, &mut elements, Layer::Background);
        elements
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
        tick_window_animations(self.space.elements(), now);

        // Frames for every window on screen, including neighbours sliding in.
        let throttle = Some(std::time::Duration::ZERO);
        let in_view: Vec<_> = self.workspace_views(now).into_iter().map(|(id, _)| id).collect();
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
