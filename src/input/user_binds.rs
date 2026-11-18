//! Keybinds from the config: `[binds.dev]` / `[binds.prod]`, entries like
//! `"Super+Left" = "snap left"`. A combo of modifiers alone (`"Super"`) is a tap: it
//! fires when they're pressed and released with nothing else in between.
//!
//! The defaults come first, then the user's entries: a later entry for the same combo
//! replaces an earlier one, and `"none"` removes it (the keys then go to apps).

use std::collections::HashMap;

use smithay::input::keyboard::{Keysym, xkb};

use super::binds::{Action, Mods, Profile};

#[derive(Debug, Clone, PartialEq)]
pub enum BoundAction {
    Builtin(Action),
    /// Run through `sh -c`.
    Spawn(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserBind {
    pub mods: Mods,
    /// `None` for a modifier-only tap.
    pub key: Option<Keysym>,
    pub action: BoundAction,
}

/// Compile bind entries in order (defaults, then the user's); bad entries are skipped
/// and reported.
pub fn compile(entries: &[(String, String)], profile: Profile) -> (Vec<UserBind>, Vec<String>) {
    let mut out: Vec<UserBind> = Vec::new();
    let mut index: HashMap<(Mods, Option<Keysym>), usize> = HashMap::new();
    let mut errors = Vec::new();
    for (combo, action) in entries {
        let (mods, key) = match parse_combo(combo, profile) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("binds: \"{combo}\": {e}"));
                continue;
            }
        };
        let action = action.trim();
        if action == "none" {
            if let Some(i) = index.remove(&(mods, key)) {
                out.remove(i);
                index.values_mut().filter(|j| **j > i).for_each(|j| *j -= 1);
            }
            continue;
        }
        let action = match parse_action(action) {
            Ok(a) => a,
            Err(e) => {
                errors.push(format!("binds: \"{combo}\": {e}"));
                continue;
            }
        };
        match index.get(&(mods, key)) {
            Some(&i) => out[i].action = action,
            None => {
                index.insert((mods, key), out.len());
                out.push(UserBind { mods, key, action });
            }
        }
    }
    (out, errors)
}

/// The bind for a key press with exactly `mods` held.
pub fn lookup(binds: &[UserBind], mods: Mods, sym: Keysym) -> Option<&UserBind> {
    binds.iter().find(|b| b.mods == mods && b.key == Some(sym))
}

/// The bind for tapping exactly `mods`.
pub fn lookup_tap(binds: &[UserBind], mods: Mods) -> Option<&BoundAction> {
    binds.iter().find(|b| b.mods == mods && b.key.is_none()).map(|b| &b.action)
}

/// Add a modifier by name to `mods`; false if `name` isn't one.
fn add_modifier(mods: &mut Mods, name: &str, profile: Profile) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "super" | "logo" | "win" | "mod4" => mods.logo = true,
        "ctrl" | "control" => mods.ctrl = true,
        "alt" | "mod1" => mods.alt = true,
        "shift" => mods.shift = true,
        // Shorthand for the profile's main modifier.
        "mod" => {
            let w = profile.window_mod();
            mods.ctrl |= w.ctrl;
            mods.alt |= w.alt;
            mods.shift |= w.shift;
            mods.logo |= w.logo;
        }
        _ => return false,
    }
    true
}

fn parse_combo(combo: &str, profile: Profile) -> Result<(Mods, Option<Keysym>), String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (last, modifiers) = parts.split_last().ok_or("empty combo")?;
    if last.is_empty() {
        return Err("no key".into());
    }
    let mut mods = Mods::default();
    for m in modifiers {
        if !add_modifier(&mut mods, m, profile) {
            return Err(format!("unknown modifier '{m}'"));
        }
    }
    // Modifiers alone: a tap.
    if add_modifier(&mut mods, last, profile) {
        return Ok((mods, None));
    }
    let sym = xkb::keysym_from_name(last, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym == Keysym::NoSymbol {
        return Err(format!("unknown key '{last}'"));
    }
    Ok((mods, Some(sym)))
}

fn parse_action(action: &str) -> Result<BoundAction, String> {
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

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    const LOGO: Mods = Mods { ctrl: false, alt: false, shift: false, logo: true };

    #[test]
    fn mod_follows_the_profile() {
        let (dev, _) = compile(&entries(&[("Mod+Return", "spawn foot")]), Profile::Dev);
        let (prod, _) = compile(&entries(&[("Mod+Return", "spawn foot")]), Profile::Prod);
        assert!(dev[0].mods.alt && !dev[0].mods.logo);
        assert!(prod[0].mods.logo && !prod[0].mods.alt);
        assert_eq!(dev[0].key, Some(Keysym::Return));
        assert_eq!(dev[0].action, BoundAction::Spawn("foot".into()));
    }

    #[test]
    fn keys_are_case_insensitive_and_match_unshifted() {
        let (b, e) = compile(&entries(&[("Super+Shift+Q", "close")]), Profile::Prod);
        assert!(e.is_empty());
        assert_eq!(b[0].key, Some(Keysym::q), "matches the unshifted keysym the filter sees");
        assert_eq!(b[0].action, BoundAction::Builtin(Action::Close));
        assert!(b[0].mods.shift && b[0].mods.logo);
    }

    #[test]
    fn modifiers_alone_are_taps() {
        let (b, e) = compile(&entries(&[("Super", "overview"), ("Shift+Alt", "overview")]), Profile::Prod);
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(lookup_tap(&b, LOGO), Some(&BoundAction::Builtin(Action::Overview)));
        let shift_alt = Mods { shift: true, alt: true, ..Default::default() };
        assert!(lookup_tap(&b, shift_alt).is_some());
        assert!(lookup(&b, LOGO, Keysym::Super_L).is_none(), "a tap isn't a key bind");
    }

    #[test]
    fn later_entries_override_and_none_removes() {
        let (b, e) = compile(
            &entries(&[
                ("Super+Left", "snap left"),
                ("Ctrl+Super+Up", "workspace up"),
                ("Super+Return", "spawn foot"),
                // The user's config, after the defaults ("Mod" is Super in prod):
                ("Mod+Left", "workspace left"),
                ("Ctrl+Mod+Up", "none"),
            ]),
            Profile::Prod,
        );
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(b.len(), 2);
        assert_eq!(lookup(&b, LOGO, Keysym::Left).unwrap().action, BoundAction::Builtin(Action::Workspace(Dir::Left)));
        assert!(lookup(&b, Mods { ctrl: true, ..LOGO }, Keysym::Up).is_none(), "removed");
        assert!(lookup(&b, LOGO, Keysym::Return).is_some(), "untouched entries stay");
    }

    #[test]
    fn errors_are_reported_and_skipped() {
        let (b, e) = compile(
            &entries(&[("Hyper+x", "close"), ("Super+nosuchkey", "close"), ("Super+x", "fly"), ("Super+y", "close")]),
            Profile::Prod,
        );
        assert_eq!(b.len(), 1, "only the valid one survives");
        assert_eq!(e.len(), 3);
    }

    #[test]
    fn lookup_matches_exact_mods() {
        let (b, _) = compile(&entries(&[("Super+q", "close")]), Profile::Prod);
        assert!(lookup(&b, LOGO, Keysym::q).is_some());
        assert!(lookup(&b, Mods { shift: true, ..LOGO }, Keysym::q).is_none());
    }
}
