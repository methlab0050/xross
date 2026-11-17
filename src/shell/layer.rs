//! wlr-layer-shell: panels, launchers, wallpapers, notifications.

use smithay::{
    desktop::{LayerSurface, PopupKind, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output, wl_surface::WlSurface},
    utils::SERIAL_COUNTER,
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{
                KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::PopupSurface,
        },
    },
};

use crate::{focus::KeyboardFocusTarget, state::XrossState};

impl WlrLayerShellHandler for XrossState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let Some(output) =
            wl_output.as_ref().and_then(Output::from_resource).or_else(|| self.space.outputs().next().cloned())
        else {
            surface.send_close();
            return;
        };
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!(?err, "failed to map layer surface");
        }
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        let _ = self.popups.track_popup(PopupKind::Xdg(popup));
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let mut had_focus = false;
        for output in self.space.outputs() {
            let mut map = layer_map_for_output(output);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                had_focus |= self.keyboard_focus_is_layer(&layer);
                map.unmap_layer(&layer);
            }
        }
        if had_focus {
            self.focus_top_window();
        }
    }
}

impl XrossState {
    fn keyboard_focus_is_layer(&self, layer: &LayerSurface) -> bool {
        matches!(
            self.seat.get_keyboard().and_then(|k| k.current_focus()),
            Some(KeyboardFocusTarget::Layer(l)) if &l == layer
        )
    }

    pub fn layer_for_surface(&self, surface: &WlSurface) -> Option<(Output, LayerSurface)> {
        self.space.outputs().find_map(|o| {
            layer_map_for_output(o)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .cloned()
                .map(|l| (o.clone(), l))
        })
    }

    /// Layer commit handling: arrange, send the initial configure, and give keyboard
    /// focus to top/overlay layers that ask for it (e.g. launchers).
    /// Returns true if `surface` was a layer surface.
    pub fn handle_layer_commit(&mut self, surface: &WlSurface) -> bool {
        let Some((output, layer)) = self.layer_for_surface(surface) else {
            return false;
        };

        let initial_configure_sent = with_states(surface, |states| {
            states.data_map.get::<LayerSurfaceData>().unwrap().lock().unwrap().initial_configure_sent
        });

        // Arrange before the initial configure so the client gets its real size.
        layer_map_for_output(&output).arrange();
        if !initial_configure_sent {
            layer.layer_surface().send_configure();
            return true;
        }

        tracing::trace!(namespace = layer.namespace(), layer = ?layer.layer(), "layer surface committed");
        let wants_focus = layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
            && matches!(layer.layer(), Layer::Top | Layer::Overlay);
        if wants_focus && !self.is_locked() && !self.keyboard_focus_is_layer(&layer) {
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, Some(KeyboardFocusTarget::Layer(layer)), SERIAL_COUNTER.next_serial());
        }
        true
    }
}
