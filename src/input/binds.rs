//! Keybinding profiles. The dev profile uses Alt as the main modifier so binds
//! work nested inside GNOME (which grabs Super). The prod profile uses Super.

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
pub enum Action {
    SpawnTerminal,
    Quit,
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

const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false, logo: false };
const CTRL_ALT: Mods = Mods { ctrl: true, alt: true, shift: false, logo: false };

pub fn lookup(_profile: Profile, mods: Mods, sym: Keysym) -> Option<Action> {
    // Binds shared by both profiles. Profile-specific binds (Alt vs Super) arrive in stage 1+.
    match (mods, sym) {
        (CTRL, Keysym::grave) => Some(Action::SpawnTerminal),
        (CTRL_ALT, Keysym::BackSpace) => Some(Action::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_grave_spawns_terminal_in_both_profiles() {
        for p in [Profile::Dev, Profile::Prod] {
            assert_eq!(lookup(p, CTRL, Keysym::grave), Some(Action::SpawnTerminal));
        }
    }

    #[test]
    fn extra_modifier_does_not_match() {
        let m = Mods { shift: true, ..CTRL };
        assert_eq!(lookup(Profile::Dev, m, Keysym::grave), None);
    }
}
