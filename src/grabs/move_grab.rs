//! Interactive move: title-bar drag, client move requests, and Mod+drag.

use smithay::{
    desktop::Window,
    input::pointer::{ButtonEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle},
};

use crate::state::XrossState;

pub struct MoveSurfaceGrab {
    pub start_data: PointerGrabStartData<XrossState>,
    pub window: Window,
    pub initial_window_location: Point<i32, Logical>,
}

impl PointerGrab<XrossState> for MoveSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut XrossState,
        handle: &mut PointerInnerHandle<'_, XrossState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus.
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        let new_location = (self.initial_window_location.to_f64() + delta).to_i32_round();
        if let Some(x11) = self.window.x11_surface() {
            let _ = x11.configure(Some(Rectangle::new(new_location, self.window.geometry().size)));
        }
        data.space.map_element(self.window.clone(), new_location, true);
    }

    fn button(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &ButtonEvent) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn unset(&mut self, _data: &mut XrossState) {}

    forward_pointer_grab!();
}
