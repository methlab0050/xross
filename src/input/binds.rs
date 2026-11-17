//! Keybinding profiles.
//!
//! The dev profile uses Alt as the window modifier and Shift+Alt as the workspace
//! modifier, so binds work nested inside GNOME (which grabs Super). The prod profile
//! uses Super and Ctrl+Super. Ctrl+` (terminal) is shared.

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
            Some("snap") => Ok(Action::Snap(dir(1)?)),
            Some("workspace") => Ok(Action::Workspace(dir(1)?)),
            Some("move-to-workspace") => Ok(Action::MoveToWorkspace(dir(1)?)),
            Some("switcher-next") => Ok(Action::SwitcherNext),
            Some("switcher-prev") => Ok(Action::SwitcherPrev),
            Some("switcher-commit") => Ok(Action::SwitcherCommit),
            Some("switcher-cancel") => Ok(Action::SwitcherCancel),
            _ => Err(format!(
                "unknown action '{s}' (expected spawn-terminal, quit, snap DIR, workspace DIR, \
                 move-to-workspace DIR, switcher-next, switcher-prev, switcher-commit, switcher-cancel, screenshot PATH)"
            )),
        }
    }
}

/// Modifier set, compared exactly (caps/num lock ignored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, logo: false };
    const CTRL: Mods = Mods { ctrl: true, ..Self::NONE };
    const ALT: Mods = Mods { alt: true, ..Self::NONE };
    const LOGO: Mods = Mods { logo: true, ..Self::NONE };
    const CTRL_ALT: Mods = Mods { ctrl: true, alt: true, ..Self::NONE };
    const SHIFT_ALT: Mods = Mods { shift: true, alt: true, ..Self::NONE };
    const CTRL_LOGO: Mods = Mods { ctrl: true, logo: true, ..Self::NONE };
    const CTRL_SHIFT_ALT: Mods = Mods { ctrl: true, shift: true, alt: true, ..Self::NONE };
    const CTRL_SHIFT_LOGO: Mods = Mods { ctrl: true, shift: true, logo: true, ..Self::NONE };

    fn with_shift(self) -> Mods {
        Mods { shift: true, ..self }
    }
}

impl Profile {
    /// Modifier for window actions (snap, switcher).
    pub fn window_mod(self) -> Mods {
        match self {
            Profile::Dev => Mods::ALT,
            Profile::Prod => Mods::LOGO,
        }
    }

    /// Modifier for workspace actions; tapped alone it toggles the overview (stage 7).
    pub fn workspace_mod(self) -> Mods {
        match self {
            Profile::Dev => Mods::SHIFT_ALT,
            Profile::Prod => Mods::CTRL_LOGO,
        }
    }

    /// Modifier for taking the focused window along to another workspace: the
    /// workspace modifier plus the one modifier it lacks (Ctrl in dev, Shift in prod).
    pub fn move_mod(self) -> Mods {
        match self {
            Profile::Dev => Mods::CTRL_SHIFT_ALT,
            Profile::Prod => Mods::CTRL_SHIFT_LOGO,
        }
    }

    /// Whether the workspace modifier is held (extra modifiers allowed).
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

fn arrow(sym: Keysym) -> Option<Dir> {
    match sym {
        Keysym::Left => Some(Dir::Left),
        Keysym::Right => Some(Dir::Right),
        Keysym::Up => Some(Dir::Up),
        Keysym::Down => Some(Dir::Down),
        _ => None,
    }
}

/// xkb turns Ctrl+Alt+Fn into XF86Switch_VT_n; `sym` must be the *modified* keysym here.
pub fn vt_switch(sym: Keysym) -> Option<Action> {
    let first = Keysym::XF86_Switch_VT_1.raw();
    let raw = sym.raw();
    (first..first + 12).contains(&raw).then(|| Action::SwitchVt((raw - first + 1) as i32))
}

/// `sym` should be the unshifted (raw) keysym, so Shift+Tab arrives as Tab.
pub fn lookup(profile: Profile, mods: Mods, sym: Keysym) -> Option<Action> {
    if mods == Mods::CTRL && sym == Keysym::grave {
        return Some(Action::SpawnTerminal);
    }
    if mods == Mods::CTRL_ALT && sym == Keysym::BackSpace {
        return Some(Action::Quit);
    }

    let wm = profile.window_mod();
    if mods == wm {
        if let Some(dir) = arrow(sym) {
            return Some(Action::Snap(dir));
        }
        if sym == Keysym::Tab {
            return Some(Action::SwitcherNext);
        }
    }
    if mods == wm.with_shift() && sym == Keysym::Tab {
        return Some(Action::SwitcherPrev);
    }
    if mods == profile.workspace_mod()
        && let Some(dir) = arrow(sym)
    {
        return Some(Action::Workspace(dir));
    }
    if mods == profile.move_mod()
        && let Some(dir) = arrow(sym)
    {
        return Some(Action::MoveToWorkspace(dir));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_grave_spawns_terminal_in_both_profiles() {
        for p in [Profile::Dev, Profile::Prod] {
            assert_eq!(lookup(p, Mods::CTRL, Keysym::grave), Some(Action::SpawnTerminal));
        }
    }

    #[test]
    fn extra_modifier_does_not_match() {
        assert_eq!(lookup(Profile::Dev, Mods::CTRL.with_shift(), Keysym::grave), None);
    }

    #[test]
    fn dev_uses_alt_and_shift_alt() {
        let p = Profile::Dev;
        assert_eq!(lookup(p, Mods::ALT, Keysym::Right), Some(Action::Snap(Dir::Right)));
        assert_eq!(lookup(p, Mods::ALT, Keysym::Up), Some(Action::Snap(Dir::Up)));
        assert_eq!(lookup(p, Mods::SHIFT_ALT, Keysym::Left), Some(Action::Workspace(Dir::Left)));
        assert_eq!(lookup(p, Mods::CTRL_SHIFT_ALT, Keysym::Right), Some(Action::MoveToWorkspace(Dir::Right)));
        assert_eq!(lookup(p, Mods::ALT, Keysym::Tab), Some(Action::SwitcherNext));
        assert_eq!(lookup(p, Mods::SHIFT_ALT, Keysym::Tab), Some(Action::SwitcherPrev));
        assert_eq!(lookup(p, Mods::LOGO, Keysym::Right), None);
    }

    #[test]
    fn vt_switch_keysyms() {
        assert_eq!(vt_switch(Keysym::XF86_Switch_VT_3), Some(Action::SwitchVt(3)));
        assert_eq!(vt_switch(Keysym::XF86_Switch_VT_12), Some(Action::SwitchVt(12)));
        assert_eq!(vt_switch(Keysym::F3), None);
    }

    #[test]
    fn prod_uses_super_and_ctrl_super() {
        let p = Profile::Prod;
        assert_eq!(lookup(p, Mods::LOGO, Keysym::Left), Some(Action::Snap(Dir::Left)));
        assert_eq!(lookup(p, Mods::CTRL_LOGO, Keysym::Down), Some(Action::Workspace(Dir::Down)));
        assert_eq!(lookup(p, Mods::CTRL_SHIFT_LOGO, Keysym::Up), Some(Action::MoveToWorkspace(Dir::Up)));
        assert_eq!(lookup(p, Mods::LOGO, Keysym::Tab), Some(Action::SwitcherNext));
        assert_eq!(lookup(p, Mods::LOGO.with_shift(), Keysym::Tab), Some(Action::SwitcherPrev));
        assert_eq!(lookup(p, Mods::ALT, Keysym::Right), None, "Alt is left to apps in prod");
    }
}
