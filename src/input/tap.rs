//! Modifier-only taps (e.g. `"Super" = "overview"` in the config), the way GNOME
//! treats a lone Super press: it fires when every modifier is released, for the
//! combination that was held, but only if nothing else happened meanwhile (no other
//! key, click, scroll or gesture).

use super::binds::Mods;

#[derive(Debug, Default)]
pub struct ModTap {
    /// Every modifier held since the last full release.
    peak: Mods,
    spoiled: bool,
}

impl ModTap {
    /// A key press. `mods` is the modifier state after it.
    pub fn press(&mut self, is_modifier_key: bool, mods: Mods) {
        if !is_modifier_key {
            self.spoil();
            return;
        }
        self.peak.ctrl |= mods.ctrl;
        self.peak.alt |= mods.alt;
        self.peak.shift |= mods.shift;
        self.peak.logo |= mods.logo;
    }

    /// Anything else happened while modifiers were held (a click, a scroll, a gesture).
    pub fn spoil(&mut self) {
        self.spoiled = true;
    }

    /// A key release (`mods` is the state after it). Once everything is released,
    /// returns the combination that was tapped, if it was a clean tap.
    pub fn release(&mut self, mods: Mods) -> Option<Mods> {
        if !mods.is_empty() {
            return None;
        }
        let tapped = (!self.spoiled && !self.peak.is_empty()).then_some(self.peak);
        *self = Self::default();
        tapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, logo: false };
    const SHIFT: Mods = Mods { shift: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };
    const SHIFT_ALT: Mods = Mods { shift: true, alt: true, ..NONE };
    const LOGO: Mods = Mods { logo: true, ..NONE };

    #[test]
    fn combination_fires_on_full_release() {
        let mut t = ModTap::default();
        t.press(true, SHIFT);
        t.press(true, SHIFT_ALT);
        assert_eq!(t.release(SHIFT), None, "still holding shift");
        assert_eq!(t.release(NONE), Some(SHIFT_ALT));
    }

    #[test]
    fn single_modifier() {
        let mut t = ModTap::default();
        t.press(true, LOGO);
        assert_eq!(t.release(NONE), Some(LOGO));
    }

    #[test]
    fn another_key_spoils_it() {
        let mut t = ModTap::default();
        t.press(true, SHIFT_ALT);
        t.press(false, SHIFT_ALT); // Shift+Alt+Right: a workspace switch
        assert_eq!(t.release(NONE), None);
    }

    #[test]
    fn a_click_spoils_it() {
        let mut t = ModTap::default();
        t.press(true, ALT);
        t.spoil();
        assert_eq!(t.release(NONE), None);
    }

    #[test]
    fn resets_after_release() {
        let mut t = ModTap::default();
        t.press(false, NONE); // typing
        assert_eq!(t.release(NONE), None);
        t.press(true, SHIFT_ALT);
        assert_eq!(t.release(NONE), Some(SHIFT_ALT));
    }
}
