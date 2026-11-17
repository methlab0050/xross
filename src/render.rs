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
        ssd::{self, DecorationStyle},
    },
    state::XrossState,
};

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
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

        for window in self.visible_windows().rev() {
            let Some(loc) = self.space.element_location(window) else {
                continue;
            };
            let geo = window.geometry();
            let content = Rectangle::new(loc, geo.size);
            if !output_geo.overlaps(content) && !output_geo.overlaps(ssd::title_rect(content)) {
                continue;
            }

            let render_loc: Point<i32, _> = (loc - geo.loc - output_geo.loc).to_physical_precise_round(scale);
            elements.extend(
                window
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, render_loc, scale, 1.0)
                    .into_iter()
                    .map(OutputRenderElement::Surface),
            );

            if has_ssd(window) {
                let style = DecorationStyle {
                    focused: focused.as_ref() == Some(window),
                    close_hovered: hovered_close.as_ref() == Some(window),
                };
                elements.extend(
                    ssd::render_elements(window, content, output_geo.loc, scale, style)
                        .into_iter()
                        .map(OutputRenderElement::Solid),
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
        let throttle = Some(std::time::Duration::ZERO);
        for window in self.visible_windows() {
            window.send_frame(output, now, throttle, |_, _| Some(output.clone()));
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
