//! Modifier-only taps (Shift+Alt in dev, Super in prod, toggle the overview), the way
//! GNOME treats a lone Super press: it fires when the modifiers are released, but only
//! if exactly the target combination was held and nothing else happened meanwhile
//! (no other key, no extra modifier, no click or scroll).

use super::binds::Mods;

#[derive(Debug, Default)]
pub struct ModTap {
    armed: bool,
    spoiled: bool,
}

fn subset(a: Mods, b: Mods) -> bool {
    (!a.ctrl || b.ctrl) && (!a.alt || b.alt) && (!a.shift || b.shift) && (!a.logo || b.logo)
}

impl ModTap {
    /// A key press. `mods` is the modifier state after it.
    pub fn press(&mut self, is_modifier_key: bool, mods: Mods, target: Mods) {
        if !is_modifier_key || !subset(mods, target) {
            self.spoil();
        } else if mods == target && !self.spoiled {
            self.armed = true;
        }
    }

    /// Anything else happened while modifiers were held (a click, a scroll, a gesture).
    pub fn spoil(&mut self) {
        self.armed = false;
        self.spoiled = true;
    }

    /// A key release. Returns true if this completes a tap.
    pub fn release(&mut self, mods: Mods) -> bool {
        if mods != Mods::default() {
            return false;
        }
        let fire = self.armed && !self.spoiled;
        *self = Self::default();
        fire
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, logo: false };
    const SHIFT: Mods = Mods { shift: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };
    const SHIFT_ALT: Mods = Mods { shift: true, alt: true, ..NONE };
    const CTRL_SHIFT_ALT: Mods = Mods { ctrl: true, shift: true, alt: true, ..NONE };

    #[test]
    fn shift_alt_tap_fires_on_full_release() {
        let mut t = ModTap::default();
        t.press(true, SHIFT, SHIFT_ALT);
        t.press(true, SHIFT_ALT, SHIFT_ALT);
        assert!(!t.release(SHIFT), "still holding shift");
        assert!(t.release(NONE));
    }

    #[test]
    fn another_key_spoils_it() {
        let mut t = ModTap::default();
        t.press(true, SHIFT_ALT, SHIFT_ALT);
        t.press(false, SHIFT_ALT, SHIFT_ALT); // Shift+Alt+Right: a workspace switch
        assert!(!t.release(NONE));
    }

    #[test]
    fn an_extra_modifier_spoils_it() {
        let mut t = ModTap::default();
        t.press(true, SHIFT_ALT, SHIFT_ALT);
        t.press(true, CTRL_SHIFT_ALT, SHIFT_ALT);
        assert!(!t.release(NONE));
    }

    #[test]
    fn a_click_spoils_it() {
        let mut t = ModTap::default();
        t.press(true, SHIFT_ALT, SHIFT_ALT);
        t.spoil();
        assert!(!t.release(NONE));
    }

    #[test]
    fn partial_combination_does_nothing() {
        let mut t = ModTap::default();
        t.press(true, ALT, SHIFT_ALT);
        assert!(!t.release(NONE));
    }

    #[test]
    fn resets_after_release() {
        let mut t = ModTap::default();
        t.press(false, NONE, SHIFT_ALT); // typing
        t.release(NONE);
        t.press(true, SHIFT_ALT, SHIFT_ALT);
        assert!(t.release(NONE));
    }
}
