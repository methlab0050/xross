//! Keyboard focus target. X11 windows must be focused through their `X11Surface`
//! (so the XWM sets X input focus / sends WM_TAKE_FOCUS), which is why keyboard focus
//! is an enum instead of a plain `WlSurface`. Pointer focus stays a `WlSurface`.

use std::borrow::Cow;

use smithay::{
    backend::input::KeyState,
    desktop::{LayerSurface, PopupKind, Window, WindowSurface},
    input::{
        Seat,
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
    },
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface},
    utils::{IsAlive, Serial},
    wayland::seat::WaylandFocus,
};

use crate::state::XrossState;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum KeyboardFocusTarget {
    Window(Window),
    Layer(LayerSurface),
    Popup(PopupKind),
    /// A session lock surface (swaylock).
    Lock(WlSurface),
}

impl KeyboardFocusTarget {
    fn inner(&self) -> &dyn KeyboardTarget<XrossState> {
        match self {
            Self::Window(w) => match w.underlying_surface() {
                WindowSurface::Wayland(t) => t.wl_surface(),
                WindowSurface::X11(s) => s,
            },
            Self::Layer(l) => l.wl_surface(),
            Self::Popup(p) => p.wl_surface(),
            Self::Lock(s) => s,
        }
    }

    pub fn window(&self) -> Option<&Window> {
        match self {
            Self::Window(w) => Some(w),
            _ => None,
        }
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Window(w) => w.alive(),
            Self::Layer(l) => l.alive(),
            Self::Popup(p) => p.alive(),
            Self::Lock(s) => s.alive(),
        }
    }
}

impl KeyboardTarget<XrossState> for KeyboardFocusTarget {
    fn enter(&self, seat: &Seat<XrossState>, data: &mut XrossState, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        self.inner().enter(seat, data, keys, serial)
    }

    fn leave(&self, seat: &Seat<XrossState>, data: &mut XrossState, serial: Serial) {
        self.inner().leave(seat, data, serial)
    }

    fn key(
        &self,
        seat: &Seat<XrossState>,
        data: &mut XrossState,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: smithay::backend::input::InputTime,
    ) {
        self.inner().key(seat, data, key, state, serial, time)
    }

    fn modifiers(&self, seat: &Seat<XrossState>, data: &mut XrossState, modifiers: ModifiersState, serial: Serial) {
        self.inner().modifiers(seat, data, modifiers, serial)
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Window(w) => w.wl_surface(),
            Self::Layer(l) => Some(Cow::Borrowed(l.wl_surface())),
            Self::Popup(p) => Some(Cow::Borrowed(p.wl_surface())),
            Self::Lock(s) => Some(Cow::Borrowed(s)),
        }
    }

    fn same_client_as(&self, object_id: &ObjectId) -> bool {
        match self {
            Self::Window(w) => w.same_client_as(object_id),
            other => other.wl_surface().is_some_and(|s| s.same_client_as(object_id)),
        }
    }
}

impl From<Window> for KeyboardFocusTarget {
    fn from(w: Window) -> Self {
        Self::Window(w)
    }
}

impl From<LayerSurface> for KeyboardFocusTarget {
    fn from(l: LayerSurface) -> Self {
        Self::Layer(l)
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(p: PopupKind) -> Self {
        Self::Popup(p)
    }
}

/// Needed by `PopupManager::grab_popup` (pointer focus is derived from the popup root).
impl From<KeyboardFocusTarget> for WlSurface {
    fn from(target: KeyboardFocusTarget) -> Self {
        target.wl_surface().map(Cow::into_owned).expect("keyboard focus target without a wl_surface")
    }
}
