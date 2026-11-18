//! Actions, bind profiles and modifiers.
//!
//! Every keybind comes from the config (`[binds.dev]` / `[binds.prod]`, see
//! `input::user_binds`); only VT switching (Ctrl+Alt+F1..F12) is built in. The dev
//! profile is Alt-based so binds work nested inside GNOME (which grabs Super); prod is
//! Super-based. The profile also picks the modifiers for mouse move/resize and for the
//! scroll stand-ins for touchpad swipes.

use smithay::input::keyboard::{Keysym, ModifiersState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Dev,
    Prod,
}

impl std::str::FromStr for Profile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dev" => Ok(Profile::Dev),
            "prod" => Ok(Profile::Prod),
            other => Err(format!("unknown bind profile '{other}' (expected dev|prod)")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SpawnTerminal,
    Quit,
    /// Ask the focused window to close.
    Close,
    /// Toggle fullscreen on the focused window.
    ToggleFullscreen,
    /// Mod+arrow. Left/Right snap to halves, Up maximizes, Down restores floating;
    /// a quick follow-up turns a half into a quadrant (see `input::combo`).
    Snap(Dir),
    /// Workspace-mod+arrow: move to the adjacent workspace, if the grid rules allow it.
    Workspace(Dir),
    /// Workspace-mod+Ctrl/Shift+arrow: take the focused window to the adjacent workspace.
    MoveToWorkspace(Dir),
    /// Mod+Tab / Mod+Shift+Tab: MRU switcher.
    SwitcherNext,
    SwitcherPrev,
    /// Releasing Mod commits; these exist for `xross msg`, where nothing is held.
    SwitcherCommit,
    SwitcherCancel,
    /// Toggle the overview (bound to tapping the workspace modifier, see `input::tap`).
    Overview,
    /// Overview navigation for `xross msg` (keys do this directly while it's open).
    OverviewMove(Dir),
    OverviewEnter,
    /// Pan the overview by screen pixels, then let go (what two/three fingers do).
    OverviewPan(i32, i32),
    OverviewPanEnd,
    /// Ctrl+Alt+F1..F12 on the udev backend.
    SwitchVt(i32),
}

impl std::str::FromStr for Dir {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Dir::Left),
            "right" => Ok(Dir::Right),
            "up" => Ok(Dir::Up),
            "down" => Ok(Dir::Down),
            other => Err(format!("unknown direction '{other}' (expected left|right|up|down)")),
        }
    }
}

/// Text form of actions, for `xross msg` (see [`crate::ipc`]): e.g. `snap left`,
/// `workspace down`, `move-to-workspace right`, `spawn-terminal`.
impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let words: Vec<&str> = s.split_whitespace().collect();
        let dir = |i: usize| words.get(i).ok_or_else(|| format!("'{s}' needs a direction"))?.parse::<Dir>();
        match words.first().copied() {
            Some("spawn-terminal") => Ok(Action::SpawnTerminal),
            Some("quit") => Ok(Action::Quit),
            Some("close") => Ok(Action::Close),
            Some("fullscreen") => Ok(Action::ToggleFullscreen),
            Some("snap") => Ok(Action::Snap(dir(1)?)),
            Some("workspace") => Ok(Action::Workspace(dir(1)?)),
            Some("move-to-workspace") => Ok(Action::MoveToWorkspace(dir(1)?)),
            Some("switcher-next") => Ok(Action::SwitcherNext),
            Some("switcher-prev") => Ok(Action::SwitcherPrev),
            Some("switcher-commit") => Ok(Action::SwitcherCommit),
            Some("switcher-cancel") => Ok(Action::SwitcherCancel),
            Some("overview") => Ok(Action::Overview),
            Some("overview-move") => Ok(Action::OverviewMove(dir(1)?)),
            Some("overview-enter") => Ok(Action::OverviewEnter),
            Some("overview-pan") => {
                let n = |i: usize| -> Result<i32, String> {
                    words.get(i).and_then(|w| w.parse().ok()).ok_or_else(|| format!("'{s}' needs DX DY"))
                };
                Ok(Action::OverviewPan(n(1)?, n(2)?))
            }
            Some("overview-pan-end") => Ok(Action::OverviewPanEnd),
            _ => Err(format!(
                "unknown action '{s}' (expected spawn CMD, spawn-terminal, quit, close, fullscreen, snap DIR, workspace DIR, \
                 move-to-workspace DIR, switcher-next, switcher-prev, switcher-commit, switcher-cancel, overview, overview-move DIR, overview-enter, overview-pan DX DY, overview-pan-end, screenshot PATH)"
            )),
        }
    }
}

/// Modifier set, compared exactly (caps/num lock ignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl From<&ModifiersState> for Mods {
    fn from(m: &ModifiersState) -> Self {
        Mods { ctrl: m.ctrl, alt: m.alt, shift: m.shift, logo: m.logo }
    }
}

impl Mods {
    const ALT: Mods = Mods { ctrl: false, alt: true, shift: false, logo: false };
    const LOGO: Mods = Mods { ctrl: false, alt: false, shift: false, logo: true };

    pub fn is_empty(self) -> bool {
        self == Mods::default()
    }

    /// Whether every modifier in `self` is held in `state` (others may be too).
    pub fn held_in(self, state: Mods) -> bool {
        (!self.ctrl || state.ctrl)
            && (!self.alt || state.alt)
            && (!self.shift || state.shift)
            && (!self.logo || state.logo)
    }
}

impl Profile {
    /// What `Mod` means in the config's binds: Alt in dev, Super in prod.
    pub fn window_mod(self) -> Mods {
        match self {
            Profile::Dev => Mods::ALT,
            Profile::Prod => Mods::LOGO,
        }
    }

    /// Whether the workspace modifier is held (extra modifiers allowed). With it, a
    /// two-finger scroll stands in for a 3-finger swipe (for nested testing).
    pub fn workspace_mod_held(self, m: &ModifiersState) -> bool {
        match self {
            Profile::Dev => m.shift && m.alt,
            Profile::Prod => m.ctrl && m.logo,
        }
    }

    /// Whether Mod+drag (move) / Mod+right-drag (resize) is active. Extra modifiers are allowed.
    pub fn drag_mod_held(self, m: &ModifiersState) -> bool {
        match self {
            Profile::Dev => m.alt,
            Profile::Prod => m.logo,
        }
    }
}

/// xkb turns Ctrl+Alt+Fn into XF86Switch_VT_n; `sym` must be the *modified* keysym here.
pub fn vt_switch(sym: Keysym) -> Option<Action> {
    let first = Keysym::XF86_Switch_VT_1.raw();
    let raw = sym.raw();
    (first..first + 12).contains(&raw).then(|| Action::SwitchVt((raw - first + 1) as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt_switch_keysyms() {
        assert_eq!(vt_switch(Keysym::XF86_Switch_VT_3), Some(Action::SwitchVt(3)));
        assert_eq!(vt_switch(Keysym::XF86_Switch_VT_12), Some(Action::SwitchVt(12)));
        assert_eq!(vt_switch(Keysym::F3), None);
    }

    #[test]
    fn held_in_allows_extra_modifiers() {
        let super_shift = Mods { logo: true, shift: true, ..Default::default() };
        assert!(Mods::LOGO.held_in(super_shift));
        assert!(!super_shift.held_in(Mods::LOGO));
        assert!(Mods::default().held_in(Mods::ALT));
    }
}
