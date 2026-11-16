use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    reexports::wayland_server::{
        Client,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface},
        seat::WaylandFocus,
        shm::{ShmHandler, ShmState},
    },
    xwayland::XWaylandClientData,
};

use crate::{
    grabs::resize_grab,
    state::{ClientState, XrossState},
};

impl CompositorHandler for XrossState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.space.elements().find(|w| w.wl_surface().is_some_and(|s| *s == root)) {
                window.on_commit();
            }
        }

        if !self.handle_layer_commit(surface) {
            self.handle_xdg_commit(surface);
        }
        resize_grab::handle_commit(&mut self.space, surface);
    }
}

impl BufferHandler for XrossState {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for XrossState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
