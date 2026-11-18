pub mod binds;
pub mod combo;
pub mod launch;
pub mod tap;
pub mod user_binds;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent, GestureEndEvent, InputBackend,
        InputEvent, InputTime, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, GrabStartData, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER, Serial},
    wayland::shell::wlr_layer::KeyboardInteractivity,
};

use crate::{focus::KeyboardFocusTarget, grabs::ResizeEdge, shell::PointerHit, state::XrossState};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// What the touchpad swipe in progress is driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeKind {
    /// 3 fingers: workspace switching.
    Workspace,
    /// 4 fingers: opening/closing the overview.
    Overview,
    /// 3 fingers in the open overview: panning, like 2.
    OverviewPan,
    Other,
}

/// Result of the keyboard filter.
enum FilterAction {
    Noop,
    Bind(binds::Action),
    /// A config bind that runs a command.
    Spawn(String),
}

impl XrossState {
    pub fn run_action(&mut self, action: binds::Action) {
        if self.is_locked() && !matches!(action, binds::Action::SwitchVt(_)) {
            return;
        }
        use binds::Action;
        tracing::debug!(?action, "running bind");
        match action {
            Action::SpawnTerminal => self.spawn(&crate::config::current().general.terminal),
            Action::Quit => self.loop_signal.stop(),
            Action::Close => {
                if let Some(w) = self.focused_window() {
                    self.close_window(&w);
                }
            }
            Action::ToggleFullscreen => {
                if let Some(w) = self.focused_window() {
                    let on = !crate::shell::snap::is_fullscreen(&w);
                    self.set_fullscreen(&w, on);
                }
            }
            Action::Snap(dir) => self.snap_focused(dir),
            Action::Workspace(dir) => self.switch_workspace(dir),
            Action::MoveToWorkspace(dir) => self.move_window_to_workspace(dir),
            Action::SwitcherNext => self.switcher_step(false),
            Action::SwitcherPrev => self.switcher_step(true),
            Action::SwitcherCommit => self.switcher_commit(),
            Action::SwitcherCancel => self.switcher_cancel(),
            Action::Overview => self.toggle_overview(),
            Action::OverviewMove(dir) => {
                use smithay::input::keyboard::Keysym;
                let sym = match dir {
                    binds::Dir::Left => Keysym::Left,
                    binds::Dir::Right => Keysym::Right,
                    binds::Dir::Up => Keysym::Up,
                    binds::Dir::Down => Keysym::Down,
                };
                self.overview_key(sym);
            }
            Action::OverviewEnter => {
                self.overview_key(smithay::input::keyboard::Keysym::Return);
            }
            Action::OverviewPan(dx, dy) => self.overview_pan(dx as f64, dy as f64, Some(self.overview_pan_idle())),
            Action::OverviewPanEnd => self.overview_pan_end(),
            Action::SwitchVt(vt) => {
                if let Some(udev) = self.udev.as_ref() {
                    udev.borrow_mut().change_vt(vt);
                }
            }
        }
        self.run_overview_pending();
    }

    /// Run a command line through `sh -c`, with an activation token so it can bring
    /// its window forward (e.g. relaunching an already-running single-instance app).
    pub fn spawn(&mut self, cmd: &str) {
        let token = self.spawn_token();
        // WAYLAND_DISPLAY (and DISPLAY once Xwayland is up) are set in our own environment.
        let res = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("XDG_ACTIVATION_TOKEN", &token)
            .env("DESKTOP_STARTUP_ID", &token)
            .spawn();
        match res {
            Ok(child) => self.children.push(child),
            Err(err) => tracing::warn!(%cmd, %err, "failed to spawn"),
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        // Any input is activity (ext-idle-notify, for swayidle and friends).
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
        self.dispatch_input_event(event);
        self.run_overview_pending();
    }

    fn dispatch_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => self.on_keyboard::<I>(event),
            InputEvent::PointerMotion { event, .. } => {
                use smithay::backend::input::PointerMotionEvent;
                self.on_pointer_relative_motion(event.delta(), event.delta_unaccel(), event.time());
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let Some(output) = self.space.outputs().next() else {
                    return;
                };
                let output_geo = self.space.output_geometry(output).unwrap();
                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
                self.on_pointer_motion(pos, Event::time(&event));
            }
            InputEvent::PointerButton { event, .. } => {
                self.on_pointer_button(event.button_code(), event.state(), Event::time(&event));
            }
            InputEvent::PointerAxis { event, .. } => self.on_pointer_axis::<I>(event),
            InputEvent::GestureSwipeBegin { event } => self.on_swipe_begin(event.fingers()),
            InputEvent::GestureSwipeUpdate { event } => {
                use smithay::backend::input::GestureSwipeUpdateEvent;
                let (dx, dy) = (event.delta_x(), event.delta_y());
                match self.active_swipe {
                    Some(SwipeKind::Workspace) => self.gesture_update(dx, dy),
                    Some(SwipeKind::Overview) => self.overview_gesture_update(dx, dy),
                    // Fingers move the content: pan the opposite way.
                    // Ends with the gesture, so no idle timeout.
                    Some(SwipeKind::OverviewPan) => self.overview_pan(-dx, -dy, None),
                    _ => {}
                }
            }
            InputEvent::GestureSwipeEnd { event } => match self.active_swipe.take() {
                Some(SwipeKind::Workspace) => self.gesture_end(event.cancelled()),
                Some(SwipeKind::Overview) => self.overview_gesture_end(event.cancelled()),
                Some(SwipeKind::OverviewPan) => self.overview_pan_end(),
                _ => {}
            },
            _ => {}
        }
    }

    fn on_keyboard<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        self.on_key(event.key_code(), event.state(), Event::time(&event));
    }

    /// A key press or release, from a keyboard (or `xross msg key`, for scripted tests).
    pub fn on_key(&mut self, keycode: smithay::input::keyboard::Keycode, key_state: KeyState, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();

        let action = self.seat.get_keyboard().unwrap().input::<FilterAction, _>(
            self,
            keycode,
            key_state,
            serial,
            time,
            |state, mods, handle| {
                // Locked: everything goes to the lock surface, except VT switching.
                if state.is_locked() {
                    if key_state == KeyState::Released && state.suppressed_keys.remove(&keycode) {
                        return FilterResult::Intercept(FilterAction::Noop);
                    }
                    if key_state == KeyState::Pressed
                        && let Some(vt) = binds::vt_switch(handle.modified_sym())
                    {
                        state.suppressed_keys.insert(keycode);
                        return FilterResult::Intercept(FilterAction::Bind(vt));
                    }
                    return FilterResult::Forward;
                }
                // Type-to-launch: hold every key back until the launcher has focus.
                if state.launch_typing_active() {
                    state.record_launch_key(keycode, key_state, handle.modified_sym().is_modifier_key());
                    return FilterResult::Intercept(FilterAction::Noop);
                }
                // Releasing the modifiers of the bind that opened the switcher commits
                // (after the filter, since committing changes keyboard focus). The
                // release itself goes through.
                let holding = match state.switcher_hold {
                    Some(hold) => hold.held_in(mods.into()),
                    // Opened by `xross msg`: the profile's modifier.
                    None => state.bind_profile.drag_mod_held(mods),
                };
                if state.switcher_active() && !holding {
                    state.switcher_commit_pending = true;
                }
                if key_state == KeyState::Pressed
                    && state.switcher_active()
                    && state.switcher_key(handle.modified_sym())
                {
                    state.suppressed_keys.insert(keycode);
                    return FilterResult::Intercept(FilterAction::Noop);
                }

                // Modifiers tapped alone: their bind (e.g. "Super" = "overview") runs
                // after the filter.
                let is_modifier = handle.modified_sym().is_modifier_key();
                match key_state {
                    KeyState::Pressed => state.mod_tap.press(is_modifier, mods.into()),
                    KeyState::Released => {
                        if let Some(tapped) = state.mod_tap.release(mods.into()) {
                            state.pending_tap = user_binds::lookup_tap(&state.user_binds, tapped).cloned();
                        }
                    }
                }
                // The open overview takes every non-modifier key (VT switching still works).
                if key_state == KeyState::Pressed && state.overview_open() && !is_modifier {
                    // Typing text (no shortcut modifiers) opens the launcher with it.
                    let m = binds::Mods::from(mods);
                    if !m.ctrl
                        && !m.alt
                        && !m.logo
                        && launch::types_text(handle.modified_sym())
                        && state.launcher().is_some()
                    {
                        state.start_launch_typing(keycode);
                        state.overview_close_pending = Some(Some(state.overview.selected));
                        return FilterResult::Intercept(FilterAction::Noop);
                    }
                    state.suppressed_keys.insert(keycode);
                    if let Some(vt) = binds::vt_switch(handle.modified_sym()) {
                        return FilterResult::Intercept(FilterAction::Bind(vt));
                    }
                    state.overview_key(handle.modified_sym());
                    return FilterResult::Intercept(FilterAction::Noop);
                }

                if key_state == KeyState::Released {
                    // Swallow the release of any key whose press we intercepted.
                    if state.suppressed_keys.remove(&keycode) {
                        return FilterResult::Intercept(FilterAction::Noop);
                    }
                    return FilterResult::Forward;
                }
                // Match on the unshifted keysym so Shift+Tab is "Tab with Shift held".
                let sym = handle.raw_latin_sym_or_raw_current_sym().unwrap_or_else(|| handle.modified_sym());
                if let Some(vt) = binds::vt_switch(handle.modified_sym()) {
                    state.suppressed_keys.insert(keycode);
                    return FilterResult::Intercept(FilterAction::Bind(vt));
                }
                let Some(bind) = user_binds::lookup(&state.user_binds, mods.into(), sym) else {
                    return FilterResult::Forward;
                };
                state.suppressed_keys.insert(keycode);
                match &bind.action {
                    user_binds::BoundAction::Builtin(a) => {
                        // The switcher stays up while this bind's modifiers (less Shift,
                        // which steps backwards) are held.
                        if matches!(a, binds::Action::SwitcherNext | binds::Action::SwitcherPrev)
                            && !state.switcher_active()
                        {
                            state.switcher_hold = Some(binds::Mods { shift: false, ..bind.mods });
                        }
                        FilterResult::Intercept(FilterAction::Bind(*a))
                    }
                    user_binds::BoundAction::Spawn(cmd) => FilterResult::Intercept(FilterAction::Spawn(cmd.clone())),
                }
            },
        );
        match action {
            Some(FilterAction::Bind(a)) => self.run_action(a),
            Some(FilterAction::Spawn(cmd)) => self.spawn(&cmd),
            _ => {}
        }
        match self.pending_tap.take() {
            Some(user_binds::BoundAction::Builtin(a)) => self.run_action(a),
            Some(user_binds::BoundAction::Spawn(cmd)) => self.spawn(&cmd),
            None => {}
        }
        if std::mem::take(&mut self.switcher_commit_pending) {
            self.switcher_commit();
        }
    }

    /// Keys while the switcher is open: Esc cancels, Enter commits, arrows move the
    /// selection. Returns whether the key was used.
    fn switcher_key(&mut self, sym: smithay::input::keyboard::Keysym) -> bool {
        use smithay::input::keyboard::Keysym;
        match sym {
            Keysym::Escape => self.switcher_cancel(),
            Keysym::Return | Keysym::KP_Enter => self.switcher_commit_pending = true,
            Keysym::Left | Keysym::Up => self.switcher_step(true),
            Keysym::Right | Keysym::Down => self.switcher_step(false),
            _ => return false,
        }
        true
    }

    pub fn on_pointer_motion(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        if self.overview_open() {
            self.overview_motion(pos);
        }
        // Hovering a switcher tile selects it (releasing the modifier or clicking commits).
        // Clients under the switcher don't see the pointer meanwhile.
        let switcher = self.switcher_visible();
        if switcher && let Some(i) = self.switcher_tile_at(pos) {
            self.switcher_select(i);
        }
        // pointer_hit finds nothing while the overview is up, so clients lose focus.
        let under = if switcher { None } else { self.pointer_hit(pos).and_then(|h| h.surface()) };
        pointer.motion(self, under, &MotionEvent { location: pos, serial, time });
        pointer.frame(self);
    }

    fn on_pointer_relative_motion(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        time: InputTime,
    ) {
        let pointer = self.seat.get_pointer().unwrap();
        let mut pos = pointer.current_location() + delta;
        // Keep the pointer on the output.
        if let Some(geo) = self.space.outputs().next().and_then(|o| self.space.output_geometry(o)) {
            pos.x = pos.x.clamp(geo.loc.x as f64, (geo.loc.x + geo.size.w) as f64 - 1.0);
            pos.y = pos.y.clamp(geo.loc.y as f64, (geo.loc.y + geo.size.h) as f64 - 1.0);
        }
        let under = self.pointer_hit(pos).and_then(|h| h.surface());
        pointer.relative_motion(
            self,
            under,
            &smithay::input::pointer::RelativeMotionEvent { delta, delta_unaccel, time },
        );
        self.on_pointer_motion(pos, time);
    }

    fn on_pointer_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        let pointer = self.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();

        // A click with modifiers held means they weren't tapped alone.
        if state == ButtonState::Pressed && self.seat.get_keyboard().unwrap().modifier_state() != Default::default() {
            self.mod_tap.spoil();
        }

        // The open overview owns the pointer: click to pick, drag windows around.
        if self.overview_open() {
            if button == BTN_LEFT {
                self.overview_button(state == ButtonState::Pressed, pointer.current_location());
            }
            match state {
                ButtonState::Pressed => self.suppressed_buttons.insert(button),
                ButtonState::Released => self.suppressed_buttons.remove(&button),
            };
            return;
        }

        if state == ButtonState::Released && self.suppressed_buttons.remove(&button) {
            return;
        }

        // While the switcher is up, clicks pick a tile and never reach clients.
        if self.switcher_visible() {
            if state == ButtonState::Pressed {
                if let Some(i) = self.switcher_tile_at(pointer.current_location()) {
                    self.switcher_select(i);
                    self.switcher_commit();
                }
                self.suppressed_buttons.insert(button);
            }
            return;
        }

        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            let pos = pointer.current_location();
            let hit = self.pointer_hit(pos);
            let mods = self.seat.get_keyboard().unwrap().modifier_state();
            if self.handle_wm_click(hit, button, pos, serial, self.bind_profile.drag_mod_held(&mods)) {
                // The compositor consumed this click; the client never sees it.
                if !pointer.is_grabbed() {
                    self.suppressed_buttons.insert(button);
                    return;
                }
            }
        }

        pointer.button(self, &ButtonEvent { button, state, serial, time });
        pointer.frame(self);
    }

    /// Focus/raise on click, plus compositor-owned interactions. Returns true if the
    /// click is consumed (a grab may have been started to receive the rest of it).
    fn handle_wm_click(
        &mut self,
        hit: Option<PointerHit>,
        button: u32,
        pos: Point<f64, Logical>,
        serial: Serial,
        drag_mod: bool,
    ) -> bool {
        let Some(hit) = hit else { return false };

        if let PointerHit::Surface { layer: Some(layer), .. } = &hit {
            if layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None {
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, Some(KeyboardFocusTarget::Layer(layer.clone())), serial);
            }
            return false;
        }

        let Some(window) = hit.window().cloned() else {
            return false;
        };
        if window.x11_surface().is_some_and(|x| x.is_override_redirect()) {
            return false;
        }
        self.focus_window(Some(&window), serial);

        let start = |button| GrabStartData { focus: None, button, location: pos };
        match hit {
            PointerHit::CloseButton(_) => {
                if button == BTN_LEFT {
                    self.close_window(&window);
                }
                true
            }
            PointerHit::TitleBar(_) if button == BTN_LEFT => {
                self.start_move(window, start(button), serial);
                true
            }
            PointerHit::TitleBar(_) => true,
            PointerHit::Surface { .. } if drag_mod && button == BTN_LEFT => {
                self.start_move(window, start(button), serial);
                true
            }
            PointerHit::Surface { .. } if drag_mod && button == BTN_RIGHT => {
                let Some(loc) = self.space.element_location(&window) else {
                    return true;
                };
                let content = smithay::utils::Rectangle::new(loc, window.geometry().size);
                let edges = ResizeEdge::from_pointer(content, pos);
                self.start_resize(window, start(button), edges, serial);
                true
            }
            PointerHit::Surface { .. } => false,
        }
    }

    fn on_swipe_begin(&mut self, fingers: u32) {
        if self.is_locked() {
            self.active_swipe = Some(SwipeKind::Other);
            return;
        }
        self.mod_tap.spoil();
        let kind = match fingers {
            // In the overview, three fingers pan like two.
            3 if self.overview_open() => SwipeKind::OverviewPan,
            3 if !self.overview_active() => {
                self.gesture_begin(false);
                SwipeKind::Workspace
            }
            4 => {
                self.overview_gesture_begin(false);
                SwipeKind::Overview
            }
            // Others aren't forwarded to clients yet.
            _ => {
                tracing::debug!(fingers, "unhandled swipe");
                SwipeKind::Other
            }
        };
        self.active_swipe = Some(kind);
    }

    /// Nested, real multi-finger swipes never reach us, so two-finger scrolls with a
    /// modifier stand in: the workspace modifier for 3-finger (workspace) swipes, the
    /// window modifier for 4-finger (overview) swipes. Returns true if the event was used.
    fn emulated_swipe<I: InputBackend>(&mut self, event: &I::PointerAxisEvent) -> bool {
        #[derive(PartialEq)]
        enum Kind {
            Workspace,
            Overview,
        }
        if event.source() != AxisSource::Finger || self.active_swipe.is_some() || self.is_locked() {
            return false;
        }
        let mods = self.seat.get_keyboard().unwrap().modifier_state();
        let kind = if self.camera.gesture.as_ref().is_some_and(|g| g.emulated) {
            Kind::Workspace
        } else if self.overview_gesture_emulated() {
            Kind::Overview
        } else if self.bind_profile.workspace_mod_held(&mods) && self.overview_active() {
            // The 3-finger stand-in pans the overview, like a plain scroll.
            return false;
        } else if self.bind_profile.workspace_mod_held(&mods) {
            Kind::Workspace
        } else if self.bind_profile.drag_mod_held(&mods) {
            Kind::Overview
        } else {
            return false;
        };
        let emulating = self.camera.gesture.as_ref().is_some_and(|g| g.emulated) || self.overview_gesture_emulated();
        self.mod_tap.spoil();

        let (h, v) = (event.amount(Axis::Horizontal).unwrap_or(0.0), event.amount(Axis::Vertical).unwrap_or(0.0));
        let stop = h == 0.0 && v == 0.0;
        if !emulating {
            if stop {
                return true;
            }
            match kind {
                Kind::Workspace => self.gesture_begin(true),
                Kind::Overview => self.overview_gesture_begin(true),
            }
        }
        // Scroll amounts move content; fingers move the opposite way (natural scrolling).
        match (kind, stop) {
            (Kind::Workspace, true) => self.gesture_end(false),
            (Kind::Workspace, false) => self.gesture_update(-h, -v),
            (Kind::Overview, true) => self.overview_gesture_end(false),
            (Kind::Overview, false) => self.overview_gesture_update(-h, -v),
        }
        true
    }

    fn on_pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        if self.emulated_swipe::<I>(&event) {
            return;
        }
        // Scrolling pans the open overview and snaps to a workspace when it stops;
        // a mouse wheel steps one workspace per notch.
        if self.overview_open() {
            let (h, v) = (event.amount(Axis::Horizontal).unwrap_or(0.0), event.amount(Axis::Vertical).unwrap_or(0.0));
            match event.source() {
                AxisSource::Wheel | AxisSource::WheelTilt => {
                    let (h120, v120) = (
                        event.amount_v120(Axis::Horizontal).unwrap_or(0.0),
                        event.amount_v120(Axis::Vertical).unwrap_or(0.0),
                    );
                    let dir = match (h120, v120) {
                        (_, v) if v > 0.0 => Some(binds::Dir::Down),
                        (_, v) if v < 0.0 => Some(binds::Dir::Up),
                        (h, _) if h > 0.0 => Some(binds::Dir::Right),
                        (h, _) if h < 0.0 => Some(binds::Dir::Left),
                        _ => None,
                    };
                    if let Some(dir) = dir {
                        self.overview_wheel_step(dir);
                    }
                }
                AxisSource::Finger if h == 0.0 && v == 0.0 => self.overview_pan_end(),
                _ => self.overview_pan(h, v, Some(self.overview_pan_idle())),
            }
            return;
        }
        let source = event.source();

        let horizontal_amount = event
            .amount(Axis::Horizontal)
            .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
        let vertical_amount = event
            .amount(Axis::Vertical)
            .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
        let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
        let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

        let mut frame = AxisFrame::new(Event::time(&event)).source(source);
        if horizontal_amount != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal_amount);
            if let Some(discrete) = horizontal_amount_discrete {
                frame = frame.v120(Axis::Horizontal, discrete as i32);
            }
        }
        if vertical_amount != 0.0 {
            frame = frame.value(Axis::Vertical, vertical_amount);
            if let Some(discrete) = vertical_amount_discrete {
                frame = frame.v120(Axis::Vertical, discrete as i32);
            }
        }

        if source == AxisSource::Finger {
            if event.amount(Axis::Horizontal) == Some(0.0) {
                frame = frame.stop(Axis::Horizontal);
            }
            if event.amount(Axis::Vertical) == Some(0.0) {
                frame = frame.stop(Axis::Vertical);
            }
        }

        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
