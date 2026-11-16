//! Builds the per-frame render element list for an output, front-to-back:
//! overlay/top layers, windows (each with its title bar), bottom/background layers.

use smithay::{
    backend::renderer::{
        element::{AsRenderElements, solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement},
        gles::GlesRenderer,
    },
    desktop::layer_map_for_output,
    output::Output,
    utils::{Point, Rectangle, Scale},
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
}

pub const CLEAR_COLOR: [f32; 4] = [0.08, 0.08, 0.10, 1.0];

impl XrossState {
    pub fn output_elements(&self, renderer: &mut GlesRenderer, output: &Output) -> Vec<OutputRenderElement> {
        let Some(output_geo) = self.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = Scale::from(output.current_scale().fractional_scale());
        let mut elements = Vec::new();

        // Must run before locking the layer map below (pointer_hit locks it too).
        let focused = self.focused_window();
        let hovered_close = self
            .pointer_hit(self.seat.get_pointer().unwrap().current_location())
            .and_then(|h| match h {
                PointerHit::CloseButton(w) => Some(w),
                _ => None,
            });

        let layers = layer_map_for_output(output);
        let push_layers = |renderer: &mut GlesRenderer, elements: &mut Vec<OutputRenderElement>, kind: Layer| {
            // layers_on() is bottom-to-top within a layer; we need front-to-back.
            for layer in layers.layers_on(kind).rev() {
                let Some(geo) = layers.layer_geometry(layer) else { continue };
                let loc = (geo.loc - layer.geometry().loc).to_physical_precise_round(scale);
                elements.extend(
                    layer
                        .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, 1.0)
                        .into_iter()
                        .map(OutputRenderElement::Surface),
                );
            }
        };

        push_layers(renderer, &mut elements, Layer::Overlay);
        push_layers(renderer, &mut elements, Layer::Top);

        for window in self.space.elements().rev() {
            let Some(loc) = self.space.element_location(window) else { continue };
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

        push_layers(renderer, &mut elements, Layer::Bottom);
        push_layers(renderer, &mut elements, Layer::Background);
        elements
    }
}
