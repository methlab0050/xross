//! Workspaces on the 2D grid ([`crate::layout::grid`]).
//!
//! Every window stays in the one [`Space`] and is tagged with the id of its workspace;
//! windows on other workspaces are simply not drawn, hit-tested or sent frame
//! callbacks. Untagged windows (X11 override-redirect menus, tooltips) are visible on
//! every workspace.

use std::{cell::Cell, time::Duration};

use smithay::{
    backend::input::InputTime,
    desktop::{Space, Window},
    utils::SERIAL_COUNTER,
};

use crate::{
    input::binds::Dir,
    layout::{
        grid::{Coord, neighbour},
        snap::{Slot, Tiling},
    },
    state::XrossState,
};

pub type WsId = u64;

/// A minimap cell: (coordinate, active, current).
pub type OsdCell = (Coord, bool, bool);
/// Grid bounds: (min x, min y, max x, max y).
pub type GridBounds = (i32, i32, i32, i32);

/// How long the workspace minimap stays up after a switch.
pub const OSD_DURATION: Duration = Duration::from_millis(900);

#[derive(Debug)]
pub struct Workspace {
    /// Stable across compaction, so the overview can animate the reshuffle.
    pub id: WsId,
    pub tiling: Tiling<Window>,
}

impl Workspace {
    pub fn new(id: WsId) -> Self {
        Self { id, tiling: Tiling::default() }
    }
}

#[derive(Default)]
struct WsTag(Cell<Option<WsId>>);

pub fn workspace_of(window: &Window) -> Option<WsId> {
    window.user_data().get::<WsTag>().and_then(|t| t.0.get())
}

fn set_workspace(window: &Window, ws: WsId) {
    window.user_data().insert_if_missing(WsTag::default);
    window.user_data().get::<WsTag>().unwrap().0.set(Some(ws));
}

fn has_windows(space: &Space<Window>, ws: WsId) -> bool {
    space.elements().any(|w| workspace_of(w) == Some(ws))
}

impl XrossState {
    pub fn current_ws(&self) -> WsId {
        self.workspaces.current_ws().id
    }

    /// Whether `window` is on the current workspace (or on all of them).
    pub fn is_visible(&self, window: &Window) -> bool {
        workspace_of(window).is_none_or(|ws| ws == self.current_ws())
    }

    /// Windows on the current workspace, bottom to top.
    pub fn visible_windows(&self) -> impl DoubleEndedIterator<Item = &Window> {
        let current = self.current_ws();
        self.space.elements().filter(move |w| workspace_of(w).is_none_or(|ws| ws == current))
    }

    /// A newly mapped window joins the current workspace.
    pub fn adopt_window(&mut self, window: &Window) {
        set_workspace(window, self.current_ws());
    }

    fn workspace(&self, ws: WsId) -> Option<&Workspace> {
        self.workspaces.iter().find(|(_, w)| w.id == ws).map(|(_, w)| w)
    }

    pub fn tiling(&self, ws: WsId) -> Option<&Tiling<Window>> {
        self.workspace(ws).map(|w| &w.tiling)
    }

    pub fn tiling_mut(&mut self, ws: WsId) -> Option<&mut Tiling<Window>> {
        self.workspaces.iter_mut().find(|(_, w)| w.id == ws).map(|(_, w)| &mut w.tiling)
    }

    /// The snap slot of a window, on whichever workspace it lives.
    pub fn slot_of(&self, window: &Window) -> Option<Slot> {
        self.tiling(workspace_of(window)?)?.slot_of(window)
    }

    pub(super) fn new_workspace(next_id: &mut WsId) -> Workspace {
        *next_id += 1;
        Workspace::new(*next_id)
    }

    /// Workspace-mod+arrow: go to the neighbouring workspace, if the rules allow it.
    pub fn switch_workspace(&mut self, dir: Dir) {
        if self.camera.gesture.is_some() {
            return;
        }
        let space = &self.space;
        let Some(to) = self.workspaces.target(dir, |ws| has_windows(space, ws.id)) else {
            tracing::debug!(?dir, from = ?self.workspaces.current(), "workspace move not allowed");
            self.show_workspace_osd();
            self.bump(dir);
            return;
        };
        self.enter_workspace(to);
        self.animate_switch();
    }

    /// Whether the grid rules allow moving from `from` in `dir`.
    pub fn can_switch_from(&self, from: Coord, dir: Dir) -> bool {
        let space = &self.space;
        self.workspaces.target_from(from, dir, |ws| has_windows(space, ws.id)).is_some()
    }

    /// Take the focused window along to the neighbouring workspace. Its snap slot comes
    /// with it (displacing windows there by the usual rules).
    pub fn move_window_to_workspace(&mut self, dir: Dir) {
        if self.camera.gesture.is_some() {
            return;
        }
        let Some(window) = self.focused_window().filter(|w| workspace_of(w).is_some()) else {
            return;
        };
        self.pin_camera();
        let to = neighbour(self.workspaces.current(), dir);
        let next_id = &mut self.next_ws_id;
        self.workspaces.switch_to(to, || Self::new_workspace(next_id));
        let to_ws = self.current_ws();
        self.transfer_window(&window, to_ws);
        self.enter_workspace(to);
        self.focus_window(Some(&window), SERIAL_COUNTER.next_serial());
        self.animate_switch();
    }

    /// Move a window to another workspace, keeping its position and snap slot (which
    /// displaces windows there by the usual rules).
    pub fn transfer_window(&mut self, window: &Window, to_ws: WsId) {
        let Some(from) = workspace_of(window) else { return };
        if from == to_ws {
            return;
        }
        let slot = self.slot_of(window);
        set_workspace(window, to_ws);
        if slot.is_some() {
            let before = self.tiling(to_ws).cloned().unwrap_or_default();
            if let Some(t) = self.tiling_mut(to_ws) {
                t.assign(window, slot);
            }
            // Its restore rect is still the one from when it was floating.
            self.apply_tiling_change(to_ws, &before, Some(window), true);
            let before = self.tiling(from).cloned().unwrap_or_default();
            if let Some(t) = self.tiling_mut(from) {
                t.remove(window);
            }
            self.apply_tiling_change(from, &before, None, false);
        }
    }

    /// The logical part of a switch: make `to` current, move keyboard and pointer focus
    /// there, show the OSD. The camera animates separately and compaction waits for it.
    pub fn enter_workspace(&mut self, to: Coord) {
        self.pin_camera();
        self.snap_combo.cancel();
        self.snap_snapshot = None;
        let next_id = &mut self.next_ws_id;
        self.workspaces.switch_to(to, || Self::new_workspace(next_id));
        tracing::debug!(current = ?to, grid = %self.grid_map(), "switched workspace");
        self.focus_top_window();
        self.refresh_pointer_focus();
        self.show_workspace_osd();
    }

    /// Drop empty workspaces and close up the grid (once the camera has settled).
    pub fn compact_workspaces(&mut self) {
        let space = &self.space;
        let dropped = self.workspaces.compact(|ws| has_windows(space, ws.id));
        if !dropped.is_empty() {
            tracing::debug!(current = ?self.workspaces.current(), grid = %self.grid_map(), "compacted workspaces");
        }
    }

    fn show_workspace_osd(&mut self) {
        self.ws_osd_until = Some(self.start_time.elapsed() + OSD_DURATION);
    }

    /// Re-send pointer focus after what's under the pointer changed without it moving.
    pub fn refresh_pointer_focus(&mut self) {
        let pos = self.seat.get_pointer().unwrap().current_location();
        self.on_pointer_motion(pos, InputTime::now());
    }

    /// The grid as text for debug logs: `#` active, `.` empty, `[ ]` current.
    pub fn grid_map(&self) -> String {
        let space = &self.space;
        let cells: Vec<(Coord, bool)> = self.workspaces.iter().map(|(c, w)| (c, has_windows(space, w.id))).collect();
        let (x0, x1) = minmax(cells.iter().map(|(c, _)| c.0));
        let (y0, y1) = minmax(cells.iter().map(|(c, _)| c.1));
        let current = self.workspaces.current();
        let mut out = String::new();
        for y in y0..=y1 {
            out.push('\n');
            for x in x0..=x1 {
                let mark = match cells.iter().find(|(c, _)| *c == (x, y)) {
                    Some((_, true)) => '#',
                    Some((_, false)) => '.',
                    None => ' ',
                };
                if (x, y) == current {
                    out.push_str(&format!("[{mark}]"));
                } else {
                    out.push_str(&format!(" {mark} "));
                }
            }
        }
        out
    }

    /// Minimap cells for the OSD: (coord, active, current), plus the grid's bounds.
    pub fn osd_cells(&self) -> (Vec<OsdCell>, GridBounds) {
        let space = &self.space;
        let current = self.workspaces.current();
        let cells: Vec<_> = self.workspaces.iter().map(|(c, w)| (c, has_windows(space, w.id), c == current)).collect();
        let (x0, x1) = minmax(cells.iter().map(|(c, ..)| c.0));
        let (y0, y1) = minmax(cells.iter().map(|(c, ..)| c.1));
        (cells, (x0, y0, x1, y1))
    }

    pub fn workspace_osd_visible(&self) -> bool {
        self.ws_osd_until.is_some_and(|t| self.start_time.elapsed() < t)
    }
}

fn minmax(it: impl Iterator<Item = i32>) -> (i32, i32) {
    it.fold((i32::MAX, i32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
}
