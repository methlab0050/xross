//! xdg-shell toplevels/popups and xdg-decoration.

use smithay::{
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window, WindowSurfaceType,
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
    },
    input::{
        Seat,
        pointer::{Focus, GrabStartData as PointerGrabStartData},
    },
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode, shell::server::xdg_toplevel,
        },
        wayland_server::{
            Resource,
            protocol::{wl_output, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Rectangle, Serial},
    wayland::{
        compositor::with_states,
        seat::WaylandFocus,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
            decoration::XdgDecorationHandler,
        },
    },
};

use crate::{
    focus::KeyboardFocusTarget,
    grabs::{MoveSurfaceGrab, ResizeSurfaceGrab},
    state::XrossState,
};

impl XdgShellHandler for XrossState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Prefer server-side decorations; clients without xdg-decoration keep drawing their own.
        surface.with_pending_state(|s| {
            s.decoration_mode = Some(DecorationMode::ServerSide);
            s.bounds = Some(self.usable_area().size);
        });
        // Mapped into the space on its first buffer commit, once its size is known.
        self.unmapped_windows.push(Window::new_wayland_window(surface));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.unmapped_windows.retain(|w| w.toplevel().is_some_and(|t| t != &surface));
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };
        let was_focused = self.focused_window().as_ref() == Some(&window);
        self.space.unmap_elem(&window);
        self.forget_window(&window);
        if was_focused {
            self.focus_top_window();
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        if let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial)
            && let Some(window) = self.window_for_surface(surface.wl_surface())
        {
            self.start_move(window, start_data, serial);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();
        if let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial)
            && let Some(window) = self.window_for_surface(surface.wl_surface())
        {
            self.start_resize(window, start_data, edges.into(), serial);
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_window_slot(&window, Some(crate::layout::snap::Slot::Maximized));
            self.apply_geometry(&window);
        } else if let Some(window) = self.unmapped_window_for(&surface) {
            self.maximize_before_map(&window);
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_window_slot(&window, None);
            // Answer even if nothing changed; the client waits for a configure.
            self.apply_geometry(&window);
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<wl_output::WlOutput>) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_fullscreen(&window, true);
        } else if let Some(window) = self.unmapped_window_for(&surface) {
            self.fullscreen_before_map(&window, true);
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_fullscreen(&window, false);
        } else if let Some(window) = self.unmapped_window_for(&surface) {
            self.fullscreen_before_map(&window, false);
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat: Seat<Self> = Seat::from_resource(&seat).unwrap();
        let kind = PopupKind::Xdg(surface);
        let Some(root) = find_popup_root_surface(&kind).ok().and_then(|root| {
            self.window_for_surface(&root).map(KeyboardFocusTarget::Window).or_else(|| {
                self.space.outputs().find_map(|o| {
                    layer_map_for_output(o)
                        .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                        .cloned()
                        .map(KeyboardFocusTarget::Layer)
                })
            })
        }) else {
            return;
        };

        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else {
            return;
        };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}

impl XdgDecorationHandler for XrossState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecorationMode::ServerSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(mode));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecorationMode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn decoration_destroyed(&mut self, toplevel: ToplevelSurface) {
        // The client falls back to drawing its own decorations.
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecorationMode::ClientSide));
    }
}

impl XrossState {
    fn unmapped_window_for(&self, surface: &ToplevelSurface) -> Option<Window> {
        self.unmapped_windows.iter().find(|w| w.toplevel() == Some(surface)).cloned()
    }

    pub fn start_move(&mut self, window: Window, start_data: PointerGrabStartData<Self>, serial: Serial) {
        let Some(initial_window_location) = self.space.element_location(&window) else {
            return;
        };
        let pointer = self.seat.get_pointer().unwrap();
        // Snapped windows un-snap once the drag passes a small threshold.
        let unsnap = self.is_snapped(&window);
        let grab = MoveSurfaceGrab { start_data, window, initial_window_location, unsnap };
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    pub fn start_resize(
        &mut self,
        window: Window,
        start_data: PointerGrabStartData<Self>,
        edges: crate::grabs::ResizeEdge,
        serial: Serial,
    ) {
        self.unsnap_in_place(&window);
        let Some(loc) = self.space.element_location(&window) else {
            return;
        };
        let initial = Rectangle::new(loc, window.geometry().size);
        let pointer = self.seat.get_pointer().unwrap();
        let grab = ResizeSurfaceGrab::start(start_data, window, edges, initial);
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// xdg-specific commit handling: initial configures, and mapping new toplevels
    /// once they have a buffer.
    pub fn handle_xdg_commit(&mut self, surface: &WlSurface) {
        if let Some(idx) = self.unmapped_windows.iter().position(|w| w.wl_surface().is_some_and(|s| &*s == surface)) {
            let window = self.unmapped_windows[idx].clone();
            let toplevel = window.toplevel().unwrap();
            let initial_configure_sent = with_states(surface, |states| {
                states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap().initial_configure_sent
            });
            if !initial_configure_sent {
                toplevel.send_configure();
                return;
            }
            window.on_commit();
            let has_buffer =
                smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| s.buffer().is_some())
                    .unwrap_or(false);
            if has_buffer {
                self.unmapped_windows.remove(idx);
                self.place_new_window(window);
            }
            return;
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(xdg)) = self.popups.find_popup(surface)
            && !xdg.is_initial_configure_sent()
        {
            // The initial configure is always allowed.
            xdg.send_configure().expect("initial configure failed");
        }
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            // Layer-shell popups: constrain to the output.
            return;
        };
        let Some(window_loc) = self.space.element_location(&window) else {
            return;
        };

        // Popups may cover panels, so constrain to the whole output.
        // The positioner target is relative to the parent's geometry.
        let Some(mut target) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) else {
            return;
        };
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

fn check_grab(
    seat: &Seat<XrossState>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<PointerGrabStartData<XrossState>> {
    let pointer = seat.get_pointer()?;
    if !pointer.has_grab(serial) {
        return None;
    }
    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }
    Some(start_data)
}
