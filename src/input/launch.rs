//! Type-to-launch from the overview: the first printable key closes the overview and
//! starts the configured launcher (`overview.launcher`). Keys typed meanwhile are held
//! back, then replayed to the launcher once it takes keyboard focus, so nothing typed
//! is lost however long it takes to start. Works with any launcher that grabs the
//! keyboard when it opens (a layer-shell surface or a window).
//!
//! Replaying goes through the keyboard's normal input path, so xkb state and the
//! modifiers clients see stay right: every recorded press/release pair leaves the
//! state as it was. Keys still held when the launcher appears reach it through the
//! focus `enter` event instead (modifiers aren't pressed twice).
//!
//! To skip the launcher's startup time, it's started (hidden) as soon as the overview
//! opens; see [`Prewarm`].

use std::time::Duration;

use smithay::{
    backend::input::KeyState,
    desktop::LayerSurface,
    input::keyboard::{FilterResult, Keycode},
    reexports::wayland_server::Resource,
    utils::{IsAlive, SERIAL_COUNTER},
};

use crate::{focus::KeyboardFocusTarget, state::XrossState};

/// Give up (and stop holding keys back) if the launcher hasn't taken focus by then.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct LaunchTyping {
    /// (key, press/release, is a modifier), in order.
    events: Vec<(Keycode, KeyState, bool)>,
    /// The launcher has been started; waiting for it to take focus.
    armed: bool,
    focus_before: Option<KeyboardFocusTarget>,
    id: u64,
}

/// Whether `sym` types text (what should start a search), not navigation.
pub fn types_text(sym: smithay::input::keyboard::Keysym) -> bool {
    sym.key_char().is_some_and(|c| !c.is_control() && c != ' ')
}

impl XrossState {
    pub fn launcher(&self) -> Option<String> {
        crate::config::current().overview.launcher.clone().filter(|l| !l.trim().is_empty())
    }

    /// While keys are being held back for the launcher.
    pub fn launch_typing_active(&self) -> bool {
        self.launch_typing.is_some()
    }

    /// The first key typed in the overview (already pressed).
    pub fn start_launch_typing(&mut self, first: Keycode) {
        let keyboard = self.seat.get_keyboard().unwrap();
        // Modifiers already held (Shift for a capital) are part of what was typed.
        let mut events: Vec<_> =
            keyboard.pressed_keys().into_iter().filter(|k| *k != first).map(|k| (k, KeyState::Pressed, true)).collect();
        events.push((first, KeyState::Pressed, false));
        self.launch_seq += 1;
        self.launch_typing = Some(LaunchTyping { events, armed: false, focus_before: None, id: self.launch_seq });
        self.mod_tap.spoil();
    }

    pub fn record_launch_key(&mut self, keycode: Keycode, state: KeyState, is_modifier: bool) {
        if let Some(lt) = self.launch_typing.as_mut() {
            lt.events.push((keycode, state, is_modifier));
        }
    }

    /// After the overview has closed: start the launcher and wait for it.
    pub fn arm_launch_typing(&mut self) {
        let Some(lt) = self.launch_typing.as_mut().filter(|lt| !lt.armed) else { return };
        let Some(launcher) = crate::config::current().overview.launcher.clone() else {
            self.launch_typing = None;
            return;
        };
        lt.armed = true;
        lt.focus_before = self.seat.get_keyboard().unwrap().current_focus();
        let id = lt.id;
        tracing::debug!(%launcher, "type-to-launch");
        // Already running in the background (started with the overview)? Show it;
        // otherwise start it now.
        if !self.reveal_prewarm() {
            self.spawn(&launcher);
        }
        let res = self.loop_handle.insert_source(
            smithay::reexports::calloop::timer::Timer::from_duration(LAUNCH_TIMEOUT),
            move |_, _, state| {
                if state.launch_typing.as_ref().is_some_and(|lt| lt.id == id) {
                    tracing::warn!("the launcher didn't take keyboard focus; dropping what was typed");
                    state.launch_typing = None;
                }
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            },
        );
        if res.is_err() {
            self.launch_typing = None;
        }
    }

    /// Keyboard focus moved: if it's the launcher, hand it the keys (once the focus
    /// change has finished).
    pub fn launch_focus_changed(&mut self, focused: Option<&KeyboardFocusTarget>) {
        let Some(lt) = self.launch_typing.as_ref().filter(|lt| lt.armed) else { return };
        if focused.is_none() || focused == lt.focus_before.as_ref() {
            return;
        }
        let id = lt.id;
        self.loop_handle.insert_idle(move |state| {
            if state.launch_typing.as_ref().is_some_and(|lt| lt.id == id) {
                state.replay_launch_typing();
            }
        });
    }

    fn replay_launch_typing(&mut self) {
        let Some(lt) = self.launch_typing.take() else { return };
        let keyboard = self.seat.get_keyboard().unwrap();
        let steps = replay_plan(&lt.events, &keyboard.pressed_keys());
        let now = smithay::backend::input::InputTime::now;
        for step in &steps {
            match *step {
                Step::Input(key, state) => {
                    keyboard.input::<(), _>(self, key, state, SERIAL_COUNTER.next_serial(), now(), |_, _, _| {
                        FilterResult::Forward
                    });
                }
                Step::Forward(key) => {
                    keyboard.input_forward(self, key, KeyState::Pressed, SERIAL_COUNTER.next_serial(), now(), false)
                }
            }
        }
        tracing::debug!(steps = steps.len(), "replayed typing to the launcher");
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step<K> {
    /// Through the keyboard's input path (updating its state, then to the launcher).
    Input(K, KeyState),
    /// A key that's still held: the keyboard already counts it as down (and would
    /// ignore a second press), so only the launcher is told. Its real release, later,
    /// reaches the launcher as usual.
    Forward(K),
}

/// What to replay for the recorded `events`, given the keys `held` right now.
fn replay_plan<K: Copy + Eq + std::hash::Hash>(
    events: &[(K, KeyState, bool)],
    held: &std::collections::HashSet<K>,
) -> Vec<Step<K>> {
    let mut out = Vec::new();
    let mut pressed = std::collections::HashSet::new();
    for (i, &(key, state, is_modifier)) in events.iter().enumerate() {
        match state {
            KeyState::Pressed => {
                // Still down: this is the key's last press and it hasn't been released.
                let released_later = events[i + 1..].iter().any(|&(k, s, _)| k == key && s == KeyState::Released);
                if held.contains(&key) && !released_later {
                    // Held modifiers are already in the state the launcher got on focus.
                    if !is_modifier {
                        out.push(Step::Forward(key));
                    }
                } else {
                    pressed.insert(key);
                    out.push(Step::Input(key, KeyState::Pressed));
                }
            }
            // Only releases of presses replayed above (not of keys pressed before).
            KeyState::Released => {
                if pressed.remove(&key) {
                    out.push(Step::Input(key, KeyState::Released));
                }
            }
        }
    }
    out
}

/// A launcher started when the overview opened, kept hidden and unfocused until
/// something is typed (so it's already up by then). Only layer-shell launchers can be
/// hidden; others still start when typing begins.
#[derive(Debug)]
pub struct Prewarm {
    /// Its process group: the surface is recognized by its client's group, which
    /// survives wrapper scripts.
    pgid: i32,
    layer: Option<LayerSurface>,
    /// Typing began before its surface appeared: show and focus it when it does.
    revealed: bool,
}

impl XrossState {
    /// Per frame: start the launcher while the overview is up, close it if the overview
    /// went away without anything typed.
    pub fn tick_prewarm(&mut self) {
        let wanted = (self.overview_open() || self.overview_gesture_active()) && !self.is_locked();
        if let Some(p) = &self.prewarm {
            let dead = p.layer.as_ref().is_some_and(|l| !l.alive());
            let handed_off = p.revealed && !self.launch_typing_active();
            if dead || handed_off {
                self.prewarm = None;
            } else if !wanted && !p.revealed {
                self.dismiss_prewarm();
            }
        }
        if !wanted {
            self.prewarm_attempted = false;
            return;
        }
        if self.prewarm.is_some() || self.prewarm_attempted || self.launch_typing_active() {
            return;
        }
        let Some(launcher) = self.launcher() else { return };
        // Once per overview opening (a launcher that crashes isn't restarted every frame).
        self.prewarm_attempted = true;
        use std::os::unix::process::CommandExt;
        match std::process::Command::new("sh").arg("-c").arg(&launcher).process_group(0).spawn() {
            Ok(child) => {
                tracing::debug!(%launcher, pid = child.id(), "launcher started in the background");
                self.prewarm = Some(Prewarm { pgid: child.id() as i32, layer: None, revealed: false });
                self.children.push(child);
            }
            Err(err) => tracing::warn!(%launcher, %err, "failed to start the launcher"),
        }
    }

    fn dismiss_prewarm(&mut self) {
        let Some(p) = self.prewarm.take() else { return };
        tracing::debug!("closing the unused launcher");
        if let Some(layer) = &p.layer {
            layer.layer_surface().send_close();
        }
        // SAFETY: plain syscall; a negative pid signals the launcher's whole group.
        unsafe { libc::kill(-p.pgid, libc::SIGTERM) };
    }

    /// The prewarmed launcher's surface while it's hidden (not drawn, not focused,
    /// not under the pointer).
    pub fn hidden_launcher(&self) -> Option<&LayerSurface> {
        self.prewarm.as_ref().filter(|p| !p.revealed).and_then(|p| p.layer.as_ref())
    }

    /// A layer surface committed: is it the prewarmed launcher, and should it stay
    /// hidden? (Recognizes it the first time.)
    pub fn claim_launcher_layer(&mut self, layer: &LayerSurface) -> bool {
        use smithay::wayland::shell::wlr_layer::KeyboardInteractivity;
        let Some(p) = self.prewarm.as_mut() else { return false };
        if p.layer.is_none() {
            let takes_keys = layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None;
            let pid = self
                .display_handle
                .get_client(layer.wl_surface().id())
                .ok()
                .and_then(|c| c.get_credentials(&self.display_handle).ok())
                .map(|c| c.pid);
            // SAFETY: plain syscall.
            let ours = pid.is_some_and(|pid| unsafe { libc::getpgid(pid) } == p.pgid);
            if !(takes_keys && ours) {
                return false;
            }
            if p.revealed {
                // Typing already began: show it right away (it takes focus as usual).
                self.prewarm = None;
                return false;
            }
            p.layer = Some(layer.clone());
        }
        p.layer.as_ref() == Some(layer) && !p.revealed
    }

    /// Typing began: show the prewarmed launcher (focusing it hands it the keys).
    /// Returns false if there's none, so the launcher has to be started now.
    fn reveal_prewarm(&mut self) -> bool {
        let Some(p) = self.prewarm.as_mut() else { return false };
        p.revealed = true;
        match p.layer.clone() {
            Some(layer) if layer.alive() => {
                self.prewarm = None;
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, Some(KeyboardFocusTarget::Layer(layer)), SERIAL_COUNTER.next_serial());
            }
            Some(_) => {
                self.prewarm = None;
                return false;
            }
            // Still starting: shown and focused when its surface appears.
            None => {}
        }
        true
    }

    /// Reap finished child processes (launched apps, launchers).
    pub fn reap_children(&mut self) {
        self.children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    }
}

#[cfg(test)]
mod tests {
    use smithay::input::keyboard::Keysym;

    use std::collections::HashSet;

    use smithay::backend::input::KeyState::{Pressed as P, Released as R};

    use super::{Step, replay_plan, types_text};

    #[test]
    fn released_keys_replay_in_order() {
        let events = [(1, P, false), (1, R, false), (2, P, false), (2, R, false)];
        let plan = replay_plan(&events, &HashSet::new());
        assert_eq!(plan, vec![Step::Input(1, P), Step::Input(1, R), Step::Input(2, P), Step::Input(2, R)]);
    }

    /// Typing fast: the first letter is still down when the launcher takes focus. The
    /// keyboard ignores a second press of a held key, so it must go straight to the
    /// launcher (this was the lost first letter).
    #[test]
    fn a_key_still_held_is_forwarded() {
        let events = [(1, P, false), (2, P, false), (2, R, false)];
        let plan = replay_plan(&events, &HashSet::from([1]));
        assert_eq!(plan, vec![Step::Forward(1), Step::Input(2, P), Step::Input(2, R)]);
    }

    #[test]
    fn same_key_twice_only_the_held_press_is_forwarded() {
        let events = [(1, P, false), (1, R, false), (1, P, false)];
        let plan = replay_plan(&events, &HashSet::from([1]));
        assert_eq!(plan, vec![Step::Input(1, P), Step::Input(1, R), Step::Forward(1)]);
    }

    #[test]
    fn modifiers() {
        // Shift held for a capital, then let go: replayed around the letter.
        let events = [(50, P, true), (1, P, false), (1, R, false), (50, R, true)];
        let plan = replay_plan(&events, &HashSet::new());
        assert_eq!(plan, vec![Step::Input(50, P), Step::Input(1, P), Step::Input(1, R), Step::Input(50, R)]);
        // Still held: the launcher already got it with the focus; not pressed again.
        let plan = replay_plan(&events[..3], &HashSet::from([50]));
        assert_eq!(plan, vec![Step::Input(1, P), Step::Input(1, R)]);
        // A release of something pressed before typing began is dropped.
        let plan = replay_plan(&[(60, R, true), (1, P, false), (1, R, false)], &HashSet::new());
        assert_eq!(plan, vec![Step::Input(1, P), Step::Input(1, R)]);
    }

    #[test]
    fn only_text_starts_a_search() {
        for sym in [Keysym::a, Keysym::A, Keysym::_1, Keysym::slash, Keysym::eacute] {
            assert!(types_text(sym), "{sym:?}");
        }
        // Navigation and the overview's own keys.
        for sym in
            [Keysym::space, Keysym::Return, Keysym::Escape, Keysym::Left, Keysym::Tab, Keysym::BackSpace, Keysym::F1]
        {
            assert!(!types_text(sym), "{sym:?}");
        }
    }
}
