pub mod binds;
pub mod combo;

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, InputTime, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
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

/// Result of the keyboard filter.
enum FilterAction {
    Noop,
    Bind(binds::Action),
}

impl XrossState {
    pub fn run_action(&mut self, action: binds::Action) {
        use binds::Action;
        tracing::debug!(?action, "running bind");
        match action {
            Action::SpawnTerminal => self.spawn("foot"),
            Action::Quit => self.loop_signal.stop(),
            Action::Snap(dir) => self.snap_focused(dir),
            Action::Workspace(_) => tracing::info!(?action, "workspaces arrive in stage 4"),
            Action::SwitcherNext | Action::SwitcherPrev => {
                tracing::info!(?action, "the window switcher arrives in stage 6")
            }
            Action::SwitchVt(vt) => {
                if let Some(udev) = self.udev.as_ref() {
                    udev.borrow_mut().change_vt(vt);
                }
            }
        }
    }

    pub fn spawn(&self, cmd: &str) {
        // WAYLAND_DISPLAY (and DISPLAY once Xwayland is up) are set in our own environment.
        if let Err(err) = std::process::Command::new(cmd).spawn() {
            tracing::warn!(%cmd, %err, "failed to spawn");
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
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
            _ => {}
        }
    }

    fn on_keyboard<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time(&event);
        let keycode = event.key_code();
        let key_state = event.state();

        let action = self.seat.get_keyboard().unwrap().input::<FilterAction, _>(
            self,
            keycode,
            key_state,
            serial,
            time,
            |state, mods, handle| {
                if key_state == KeyState::Released {
                    // Swallow the release of any key whose press we intercepted.
                    if state.suppressed_keys.remove(&keycode) {
                        return FilterResult::Intercept(FilterAction::Noop);
                    }
                    return FilterResult::Forward;
                }
                // Match on the unshifted keysym so Shift+Tab is "Tab with Shift held".
                let sym = handle.raw_latin_sym_or_raw_current_sym().unwrap_or_else(|| handle.modified_sym());
                let action = binds::vt_switch(handle.modified_sym())
                    .or_else(|| binds::lookup(state.bind_profile, mods.into(), sym));
                match action {
                    Some(a) => {
                        state.suppressed_keys.insert(keycode);
                        FilterResult::Intercept(FilterAction::Bind(a))
                    }
                    None => FilterResult::Forward,
                }
            },
        );
        if let Some(FilterAction::Bind(a)) = action {
            self.run_action(a);
        }
    }

    fn on_pointer_motion(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        let under = self.pointer_hit(pos).and_then(|h| h.surface());
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

        if state == ButtonState::Released && self.suppressed_buttons.remove(&button) {
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

    fn on_pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
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
