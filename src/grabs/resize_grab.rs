//! Interactive resize for both Wayland and X11 windows.
//!
//! Wayland clients pick their final size asynchronously, so when resizing from the
//! top/left edges the window location is fixed up on commit (see [`handle_commit`])
//! to keep the opposite edge anchored.

use std::cell::RefCell;

use smithay::{
    desktop::{Space, Window, WindowSurface},
    input::pointer::{ButtonEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{compositor::with_states, seat::WaylandFocus, shell::xdg::SurfaceCachedState},
    xwayland::xwm::ResizeEdge as X11ResizeEdge,
};

use crate::state::XrossState;

const MIN_SIZE: (i32, i32) = (120, 60);

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResizeEdge: u32 {
        const TOP    = 0b0001;
        const BOTTOM = 0b0010;
        const LEFT   = 0b0100;
        const RIGHT  = 0b1000;
        const TOP_LEFT = Self::TOP.bits() | Self::LEFT.bits();
        const BOTTOM_LEFT = Self::BOTTOM.bits() | Self::LEFT.bits();
        const TOP_RIGHT = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
    }
}

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        Self::from_bits(x as u32).unwrap_or(Self::BOTTOM_RIGHT)
    }
}

impl From<X11ResizeEdge> for ResizeEdge {
    fn from(x: X11ResizeEdge) -> Self {
        match x {
            X11ResizeEdge::Top => Self::TOP,
            X11ResizeEdge::Bottom => Self::BOTTOM,
            X11ResizeEdge::Left => Self::LEFT,
            X11ResizeEdge::Right => Self::RIGHT,
            X11ResizeEdge::TopLeft => Self::TOP_LEFT,
            X11ResizeEdge::TopRight => Self::TOP_RIGHT,
            X11ResizeEdge::BottomLeft => Self::BOTTOM_LEFT,
            X11ResizeEdge::BottomRight => Self::BOTTOM_RIGHT,
        }
    }
}

impl ResizeEdge {
    /// Edges for a Mod+right-drag, chosen by which third of the window the pointer is in.
    /// The center picks bottom-right.
    pub fn from_pointer(content: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> Self {
        let rel_x = (pos.x - content.loc.x as f64) / content.size.w.max(1) as f64;
        let rel_y = (pos.y - content.loc.y as f64) / content.size.h.max(1) as f64;
        let mut edges = ResizeEdge::empty();
        if rel_x < 1.0 / 3.0 {
            edges |= ResizeEdge::LEFT;
        } else if rel_x > 2.0 / 3.0 {
            edges |= ResizeEdge::RIGHT;
        }
        if rel_y < 1.0 / 3.0 {
            edges |= ResizeEdge::TOP;
        } else if rel_y > 2.0 / 3.0 {
            edges |= ResizeEdge::BOTTOM;
        }
        if edges.is_empty() { ResizeEdge::BOTTOM_RIGHT } else { edges }
    }
}

/// Resize bookkeeping stored on the window so commits can re-anchor it.
#[derive(Debug, Clone, Copy, Default)]
enum ResizeState {
    #[default]
    Idle,
    Resizing { edges: ResizeEdge, initial: Rectangle<i32, Logical> },
    /// Grab released; fix up the location on the next commit, then go idle.
    WaitingForLastCommit { edges: ResizeEdge, initial: Rectangle<i32, Logical> },
}

fn with_resize_state<T>(window: &Window, f: impl FnOnce(&mut ResizeState) -> T) -> T {
    window.user_data().insert_if_missing(|| RefCell::new(ResizeState::default()));
    f(&mut window.user_data().get::<RefCell<ResizeState>>().unwrap().borrow_mut())
}

/// Compute the new content rect for a drag of `delta` from `initial`.
pub fn resized_rect(
    initial: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    delta: Point<f64, Logical>,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let (mut w, mut h) = (initial.size.w as f64, initial.size.h as f64);
    if edges.contains(ResizeEdge::LEFT) {
        w -= delta.x;
    } else if edges.contains(ResizeEdge::RIGHT) {
        w += delta.x;
    }
    if edges.contains(ResizeEdge::TOP) {
        h -= delta.y;
    } else if edges.contains(ResizeEdge::BOTTOM) {
        h += delta.y;
    }
    let clamp = |v: f64, lo: i32, hi: i32| {
        let hi = if hi > 0 { hi } else { i32::MAX };
        (v.round() as i32).clamp(lo.max(1), hi.max(lo.max(1)))
    };
    let size = Size::from((clamp(w, min.w, max.w), clamp(h, min.h, max.h)));
    Rectangle::new(anchored_location(initial, edges, size), size)
}

/// Keep the edge opposite to the dragged one fixed.
fn anchored_location(
    initial: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let mut loc = initial.loc;
    if edges.contains(ResizeEdge::LEFT) {
        loc.x = initial.loc.x + initial.size.w - size.w;
    }
    if edges.contains(ResizeEdge::TOP) {
        loc.y = initial.loc.y + initial.size.h - size.h;
    }
    loc
}

fn size_limits(window: &Window) -> (Size<i32, Logical>, Size<i32, Logical>) {
    let (mut min, mut max) = (Size::from(MIN_SIZE), Size::from((0, 0)));
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => with_states(t.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            min = Size::from((current.min_size.w.max(MIN_SIZE.0), current.min_size.h.max(MIN_SIZE.1)));
            max = current.max_size;
        }),
        WindowSurface::X11(s) => {
            if let Some(m) = s.min_size() {
                min = Size::from((m.w.max(MIN_SIZE.0), m.h.max(MIN_SIZE.1)));
            }
            if let Some(m) = s.max_size() {
                max = m;
            }
        }
    }
    (min, max)
}

pub struct ResizeSurfaceGrab {
    start_data: PointerGrabStartData<XrossState>,
    window: Window,
    edges: ResizeEdge,
    initial: Rectangle<i32, Logical>,
    limits: (Size<i32, Logical>, Size<i32, Logical>),
}

impl ResizeSurfaceGrab {
    /// `initial` is the window's content rect in global coordinates.
    pub fn start(
        start_data: PointerGrabStartData<XrossState>,
        window: Window,
        edges: ResizeEdge,
        initial: Rectangle<i32, Logical>,
    ) -> Self {
        with_resize_state(&window, |s| *s = ResizeState::Resizing { edges, initial });
        if let Some(t) = window.toplevel() {
            t.with_pending_state(|s| {
                s.states.set(xdg_toplevel::State::Resizing);
            });
            t.send_pending_configure();
        }
        let limits = size_limits(&window);
        Self { start_data, window, edges, initial, limits }
    }
}

impl PointerGrab<XrossState> for ResizeSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut XrossState,
        handle: &mut PointerInnerHandle<'_, XrossState>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        let rect = resized_rect(self.initial, self.edges, delta, self.limits.0, self.limits.1);

        match self.window.underlying_surface() {
            WindowSurface::Wayland(t) => {
                t.with_pending_state(|s| s.size = Some(rect.size));
                t.send_pending_configure();
            }
            WindowSurface::X11(x11) => {
                // X11 resizes synchronously enough that we can move immediately.
                let _ = x11.configure(Some(rect));
                data.space.map_element(self.window.clone(), rect.loc, true);
            }
        }
    }

    fn button(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &ButtonEvent) {
        handle.button(data, event);
        if !handle.current_pressed().is_empty() {
            return;
        }
        handle.unset_grab(self, data, event.serial, event.time, true);

        if let Some(t) = self.window.toplevel() {
            t.with_pending_state(|s| {
                s.states.unset(xdg_toplevel::State::Resizing);
            });
            t.send_pending_configure();
            let (edges, initial) = (self.edges, self.initial);
            with_resize_state(&self.window, |s| *s = ResizeState::WaitingForLastCommit { edges, initial });
        } else {
            with_resize_state(&self.window, |s| *s = ResizeState::Idle);
        }
    }

    fn unset(&mut self, _data: &mut XrossState) {}

    forward_pointer_grab!();
}

/// Called on every surface commit: re-anchor windows being resized from the top/left.
pub fn handle_commit(space: &mut Space<Window>, surface: &WlSurface) {
    let Some(window) = space
        .elements()
        .find(|w| w.wl_surface().is_some_and(|s| &*s == surface))
        .cloned()
    else {
        return;
    };

    let state = with_resize_state(&window, |s| *s);
    let (edges, initial, done) = match state {
        ResizeState::Idle => return,
        ResizeState::Resizing { edges, initial } => (edges, initial, false),
        ResizeState::WaitingForLastCommit { edges, initial } => (edges, initial, true),
    };

    if window.x11_surface().is_none() && edges.intersects(ResizeEdge::TOP_LEFT) {
        let loc = anchored_location(initial, edges, window.geometry().size);
        space.map_element(window.clone(), loc, false);
    }
    if done {
        with_resize_state(&window, |s| *s = ResizeState::Idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Rectangle<i32, Logical> {
        Rectangle::new((100, 100).into(), (400, 300).into())
    }
    fn no_max() -> Size<i32, Logical> {
        Size::from((0, 0))
    }

    #[test]
    fn bottom_right_grows_without_moving() {
        let out = resized_rect(r(), ResizeEdge::BOTTOM_RIGHT, (50.0, 20.0).into(), MIN_SIZE.into(), no_max());
        assert_eq!(out, Rectangle::new((100, 100).into(), (450, 320).into()));
    }

    #[test]
    fn top_left_keeps_bottom_right_anchored() {
        let out = resized_rect(r(), ResizeEdge::TOP_LEFT, (50.0, 20.0).into(), MIN_SIZE.into(), no_max());
        assert_eq!(out, Rectangle::new((150, 120).into(), (350, 280).into()));
    }

    #[test]
    fn clamps_to_min_size_and_stays_anchored() {
        let out = resized_rect(r(), ResizeEdge::LEFT, (1000.0, 0.0).into(), MIN_SIZE.into(), no_max());
        assert_eq!(out.size.w, MIN_SIZE.0);
        assert_eq!(out.loc.x + out.size.w, 500, "right edge stays put");
    }

    #[test]
    fn mod_drag_edge_selection() {
        let c = r();
        assert_eq!(ResizeEdge::from_pointer(c, (110.0, 110.0).into()), ResizeEdge::TOP_LEFT);
        assert_eq!(ResizeEdge::from_pointer(c, (490.0, 250.0).into()), ResizeEdge::RIGHT);
        assert_eq!(ResizeEdge::from_pointer(c, (300.0, 250.0).into()), ResizeEdge::BOTTOM_RIGHT);
    }
}
