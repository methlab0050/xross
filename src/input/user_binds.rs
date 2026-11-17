//! Keybinds from the config's `[binds]` table: `"Mod+Shift+Return" = "spawn foot"`.
//! They are checked before the built-in profile binds, so they can override them.

use std::collections::BTreeMap;

use smithay::input::keyboard::{Keysym, xkb};

use super::binds::{Action, Mods, Profile};

#[derive(Debug, Clone, PartialEq)]
pub enum BoundAction {
    Builtin(Action),
    /// Run through `sh -c`.
    Spawn(String),
    /// `"none"`: swallow the keys, disabling a built-in bind.
    Disabled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserBind {
    pub mods: Mods,
    pub sym: Keysym,
    pub action: BoundAction,
}

/// Compile the `[binds]` table; bad entries are skipped and reported.
pub fn compile(binds: &BTreeMap<String, String>, profile: Profile) -> (Vec<UserBind>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (combo, action) in binds {
        match (parse_combo(combo, profile), parse_action(action)) {
            (Ok((mods, sym)), Ok(action)) => out.push(UserBind { mods, sym, action }),
            (Err(e), _) | (_, Err(e)) => errors.push(format!("binds.\"{combo}\": {e}")),
        }
    }
    (out, errors)
}

pub fn lookup(binds: &[UserBind], mods: Mods, sym: Keysym) -> Option<&BoundAction> {
    binds.iter().find(|b| b.mods == mods && b.sym == sym).map(|b| &b.action)
}

fn parse_combo(combo: &str, profile: Profile) -> Result<(Mods, Keysym), String> {
    let mut parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let key = parts.pop().filter(|k| !k.is_empty()).ok_or("no key")?;
    let mut mods = Mods::default();
    for m in parts {
        match m.to_ascii_lowercase().as_str() {
            "super" | "logo" | "win" | "mod4" => mods.logo = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "mod1" => mods.alt = true,
            "shift" => mods.shift = true,
            "mod" => {
                let w = profile.window_mod();
                mods.ctrl |= w.ctrl;
                mods.alt |= w.alt;
                mods.shift |= w.shift;
                mods.logo |= w.logo;
            }
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    let sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym == Keysym::NoSymbol {
        return Err(format!("unknown key '{key}'"));
    }
    Ok((mods, sym))
}

fn parse_action(action: &str) -> Result<BoundAction, String> {
    let action = action.trim();
    if action == "none" {
        return Ok(BoundAction::Disabled);
    }
    if let Some(cmd) = action.strip_prefix("spawn ") {
        let cmd = cmd.trim();
        return if cmd.is_empty() { Err("spawn needs a command".into()) } else { Ok(BoundAction::Spawn(cmd.into())) };
    }
    action.parse::<Action>().map(BoundAction::Builtin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::binds::Dir;

    fn binds(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn mod_follows_the_profile() {
        let (dev, _) = compile(&binds(&[("Mod+Return", "spawn foot")]), Profile::Dev);
        let (prod, _) = compile(&binds(&[("Mod+Return", "spawn foot")]), Profile::Prod);
        assert!(dev[0].mods.alt && !dev[0].mods.logo);
        assert!(prod[0].mods.logo && !prod[0].mods.alt);
        assert_eq!(dev[0].sym, Keysym::Return);
        assert_eq!(dev[0].action, BoundAction::Spawn("foot".into()));
    }

    #[test]
    fn keys_are_case_insensitive_and_match_unshifted() {
        let (b, e) = compile(&binds(&[("Super+Shift+Q", "close")]), Profile::Prod);
        assert!(e.is_empty());
        assert_eq!(b[0].sym, Keysym::q, "matches the unshifted keysym the filter sees");
        assert_eq!(b[0].action, BoundAction::Builtin(Action::Close));
        assert!(b[0].mods.shift && b[0].mods.logo);
    }

    #[test]
    fn builtin_actions_and_none() {
        let (b, _) = compile(&binds(&[("Ctrl+grave", "none"), ("Super+h", "workspace left")]), Profile::Prod);
        assert!(b.iter().any(|x| x.action == BoundAction::Disabled));
        assert!(b.iter().any(|x| x.action == BoundAction::Builtin(Action::Workspace(Dir::Left))));
    }

    #[test]
    fn errors_are_reported_and_skipped() {
        let (b, e) = compile(
            &binds(&[("Hyper+x", "close"), ("Super+nosuchkey", "close"), ("Super+x", "fly"), ("Super+y", "close")]),
            Profile::Prod,
        );
        assert_eq!(b.len(), 1, "only the valid one survives");
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn lookup_matches_exact_mods() {
        let (b, _) = compile(&binds(&[("Super+q", "close")]), Profile::Prod);
        let super_only = Mods { logo: true, ..Default::default() };
        assert!(lookup(&b, super_only, Keysym::q).is_some());
        assert!(lookup(&b, Mods { shift: true, ..super_only }, Keysym::q).is_none());
    }
}
