//! XWayland: spawning the server, and the X11 window manager (XwmHandler).

use std::{os::unix::io::OwnedFd, process::Stdio};

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle, SERIAL_COUNTER},
    wayland::{
        selection::{
            SelectionTarget,
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
                set_primary_selection,
            },
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{Reorder, ResizeEdge as X11ResizeEdge, XwmId},
    },
};

use crate::{focus::KeyboardFocusTarget, state::XrossState};

impl XrossState {
    pub fn start_xwayland(&mut self) {
        let (xwayland, client) = match XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            std::iter::empty::<String>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        ) {
            Ok(x) => x,
            Err(err) => {
                tracing::error!(?err, "failed to start Xwayland; X11 apps will not work");
                return;
            }
        };

        let display_handle = self.display_handle.clone();
        let loop_handle = self.loop_handle.clone();
        let res = self.loop_handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => {
                match X11Wm::start_wm(loop_handle.clone(), &display_handle, x11_socket, client.clone()) {
                    Ok(wm) => {
                        state.xwm = Some(wm);
                        state.xdisplay = Some(display_number);
                        // Children (and spawned binds) get DISPLAY from our environment.
                        unsafe { std::env::set_var("DISPLAY", format!(":{display_number}")) };
                        tracing::info!(display = display_number, "Xwayland ready");
                        state.run_pending_startup_commands();
                    }
                    Err(err) => tracing::error!(?err, "failed to attach X11 window manager"),
                }
            }
            XWaylandEvent::Error => {
                tracing::warn!("Xwayland crashed on startup");
                state.run_pending_startup_commands();
            }
        });
        if let Err(err) = res {
            tracing::error!(%err, "failed to insert Xwayland source");
        }
    }

    pub fn window_for_x11(&self, surface: &X11Surface) -> Option<Window> {
        self.space.elements().find(|w| w.x11_surface() == Some(surface)).cloned()
    }
}

impl XWaylandShellHandler for XrossState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

impl XwmHandler for XrossState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().unwrap()
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Err(err) = surface.set_mapped(true) {
            tracing::warn!(?err, "failed to map X11 window");
            return;
        }
        // _NET_WM_STATE set before mapping (e.g. `xterm -maximized`) arrives as state, not requests.
        let (maximized, fullscreen) = (surface.is_maximized(), surface.is_fullscreen());
        let window = Window::new_x11_window(surface);
        self.place_new_window(window.clone());
        if fullscreen {
            self.set_fullscreen(&window, true);
        } else if maximized {
            self.set_window_slot(&window, Some(crate::layout::snap::Slot::Maximized));
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        // Menus, tooltips, etc. position themselves.
        let loc = surface.geometry().loc;
        self.space.map_element(Window::new_x11_window(surface), loc, false);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.window_for_x11(&surface) {
            let was_focused = self.focused_window().as_ref() == Some(&window);
            self.space.unmap_elem(&window);
            self.forget_window(&window);
            if was_focused {
                self.focus_top_window();
            }
        }
        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, _surface: X11Surface) {}

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // Snapped windows keep the geometry we gave them.
        if let Some(window) = self.window_for_x11(&surface)
            && self.is_snapped(&window)
        {
            self.apply_geometry(&window);
            return;
        }
        let mut geo = surface.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        // Unmapped windows may pick their position; mapped ones are placed by us.
        if self.window_for_x11(&surface).is_none() {
            if let Some(x) = x {
                geo.loc.x = x;
            }
            if let Some(y) = y {
                geo.loc.y = y;
            }
        }
        let _ = surface.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        // Move without raising; stacking only changes on focus.
        if let Some(window) = self.window_for_x11(&surface) {
            self.space.relocate_element(&window, geometry.loc);
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.window_for_x11(&surface) {
            self.set_window_slot(&window, Some(crate::layout::snap::Slot::Maximized));
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.window_for_x11(&surface) {
            self.set_window_slot(&window, None);
            self.apply_geometry(&window);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.window_for_x11(&surface) {
            self.set_fullscreen(&window, true);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.window_for_x11(&surface) {
            self.set_fullscreen(&window, false);
        }
    }

    fn resize_request(&mut self, _xwm: XwmId, surface: X11Surface, _button: u32, edges: X11ResizeEdge) {
        let pointer = self.seat.get_pointer().unwrap();
        let (Some(start_data), Some(window)) = (pointer.grab_start_data(), self.window_for_x11(&surface)) else {
            return;
        };
        self.start_resize(window, start_data, edges.into(), SERIAL_COUNTER.next_serial());
    }

    /// `_NET_ACTIVE_WINDOW`: X11's way of asking to be brought forward (X11 apps don't
    /// use xdg-activation). X11 has no reliable focus-stealing protection, so it's granted.
    fn active_window_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        _timestamp: u32,
        _currently_active: Option<X11Surface>,
    ) {
        if let Some(window) = self.window_for_x11(&surface) {
            self.activate_window(&window, "_NET_ACTIVE_WINDOW");
        }
    }

    fn move_request(&mut self, _xwm: XwmId, surface: X11Surface, _button: u32) {
        let pointer = self.seat.get_pointer().unwrap();
        let (Some(start_data), Some(window)) = (pointer.grab_start_data(), self.window_for_x11(&surface)) else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        self.start_move(window, start_data, serial);
    }

    fn allow_selection_access(&mut self, xwm: XwmId, selection: SelectionTarget) -> bool {
        // Only the focused X11 client may read the clipboard.
        let allowed = matches!(
            self.seat.get_keyboard().and_then(|k| k.current_focus()),
            Some(KeyboardFocusTarget::Window(w))
                if w.x11_surface().and_then(|s| s.xwm_id()) == Some(xwm)
        );
        tracing::debug!(?selection, allowed, "X11 selection access");
        allowed
    }

    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let res = match selection {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection(&self.seat, mime_type, fd).map_err(|e| e.to_string())
            }
            SelectionTarget::Primary => {
                request_primary_client_selection(&self.seat, mime_type, fd).map_err(|e| e.to_string())
            }
        };
        if let Err(err) = res {
            tracing::warn!(%err, ?selection, "failed to send Wayland selection to Xwayland");
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        match selection {
            SelectionTarget::Clipboard => set_data_device_selection(&self.display_handle, &self.seat, mime_types, ()),
            SelectionTarget::Primary => set_primary_selection(&self.display_handle, &self.seat, mime_types, ()),
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        self.xwm = None;
        self.xdisplay = None;
    }
}
