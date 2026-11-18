mod compositor;
mod protocols;

mod wlr_screencopy;

pub use protocols::{CaptureTarget, PendingCapture, SessionLock};
pub use wlr_screencopy::WlrScreencopyState;

use std::os::unix::io::OwnedFd;

use smithay::{
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{CursorImageStatus, Focus},
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::Serial,
    wayland::{
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        seat::WaylandFocus,
        selection::{
            SelectionHandler, SelectionSource, SelectionTarget,
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState, set_primary_focus},
        },
    },
};

use crate::{focus::KeyboardFocusTarget, state::XrossState};

impl SeatHandler for XrossState {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<XrossState> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
        let dh = &self.display_handle;
        let client = focused.and_then(WaylandFocus::wl_surface).and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
        self.launch_focus_changed(focused);
    }
}

impl PointerConstraintsHandler for XrossState {}

impl SelectionHandler for XrossState {
    type SelectionUserData = ();

    // Bridge Wayland selections to X11 clients.
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(err) = xwm.new_selection(ty, source.map(|s| s.mime_types()))
        {
            tracing::warn!(?err, ?ty, "failed to set Xwayland selection");
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(err) = xwm.send_selection(ty, mime_type, fd)
        {
            tracing::warn!(?err, "failed to send X11 selection to Wayland");
        }
    }
}

impl DataDeviceHandler for XrossState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl PrimarySelectionHandler for XrossState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl smithay::wayland::selection::wlr_data_control::DataControlHandler for XrossState {
    fn data_control_state(&mut self) -> &mut smithay::wayland::selection::wlr_data_control::DataControlState {
        &mut self.wlr_data_control_state
    }
}

impl smithay::wayland::selection::ext_data_control::DataControlHandler for XrossState {
    fn data_control_state(&mut self) -> &mut smithay::wayland::selection::ext_data_control::DataControlState {
        &mut self.ext_data_control_state
    }
}

impl DndGrabHandler for XrossState {}
impl WaylandDndGrabHandler for XrossState {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let ptr = seat.get_pointer().unwrap();
                let start_data = ptr.grab_start_data().unwrap();
                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                ptr.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => source.cancel(),
        }
    }
}

impl OutputHandler for XrossState {}

smithay::delegate_dispatch2!(XrossState);
