//! Daily-driver protocols: xdg-activation, fractional scale, idle inhibit/notify,
//! session lock, and screen capture (ext-image-copy-capture). Cursor-shape, viewporter
//! and presentation-time need no handlers; their globals are created in `state.rs`.

use smithay::{
    input::{Seat, tablet::TabletSeatHandler},
    output::{Output, WeakOutput},
    reexports::wayland_server::{
        Resource,
        protocol::{wl_output::WlOutput, wl_shm, wl_surface::WlSurface},
    },
    utils::SERIAL_COUNTER,
    wayland::{
        compositor::with_states,
        fractional_scale::{FractionalScaleHandler, with_fractional_scale},
        idle_inhibit::IdleInhibitHandler,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler, OutputCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, CaptureFailureReason, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session,
            SessionRef,
        },
        session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
    },
};

use crate::{focus::KeyboardFocusTarget, shell::workspace::workspace_of, state::XrossState};

// --- xdg-activation ---------------------------------------------------------------

impl XdgActivationHandler for XrossState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    /// Only tokens tied to a recent input event on our seat are valid (no focus stealing).
    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let Some((serial, seat)) = data.serial else { return false };
        let keyboard = self.seat.get_keyboard().unwrap();
        Seat::from_resource(&seat) == Some(self.seat.clone())
            && keyboard.last_enter().is_some_and(|last| serial.is_no_older_than(&last))
    }

    fn request_activation(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        if data.timestamp.elapsed().as_secs() >= 10 || self.is_locked() {
            return;
        }
        let Some(window) = self.window_for_surface(&surface) else { return };
        tracing::debug!("xdg-activation request granted");
        // A window on another workspace brings us there.
        let elsewhere = workspace_of(&window)
            .filter(|ws| *ws != self.current_ws())
            .and_then(|ws| self.workspaces.iter().find(|(_, w)| w.id == ws).map(|(c, _)| c));
        if let Some(coord) = elsewhere {
            if self.overview_open() {
                self.close_overview(Some(coord));
            } else {
                self.enter_workspace(coord);
                self.animate_switch();
            }
        }
        self.focus_window(Some(&window), SERIAL_COUNTER.next_serial());
    }
}

// Needed by cursor-shape (tablet tools could set cursors too); nothing to do.
impl TabletSeatHandler for XrossState {
    type ToolFocus = WlSurface;
}

// --- Fractional scale -------------------------------------------------------------

impl FractionalScaleHandler for XrossState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // One output for now, so its scale is everyone's preferred scale.
        let Some(scale) = self.space.outputs().next().map(|o| o.current_scale().fractional_scale()) else {
            return;
        };
        with_states(&surface, |states| {
            with_fractional_scale(states, |fs| fs.set_preferred_scale(scale));
        });
    }
}

// --- Idle -------------------------------------------------------------------------

impl IdleNotifierHandler for XrossState {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

impl IdleInhibitHandler for XrossState {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.insert(surface);
        self.update_idle_inhibit();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.remove(&surface);
        self.update_idle_inhibit();
    }
}

impl XrossState {
    fn update_idle_inhibit(&mut self) {
        self.idle_inhibitors.retain(|s| s.is_alive());
        let inhibited = !self.idle_inhibitors.is_empty();
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }
}

// --- Session lock -----------------------------------------------------------------

/// ext-session-lock state. While locked (or about to be), nothing but the lock surface is
/// drawn or receives input; if the locking client dies, the screen stays locked.
#[derive(Default)]
pub struct SessionLock {
    /// Locked as soon as a frame without any windows has been drawn.
    pending: Option<SessionLocker>,
    locked: bool,
    pub surface: Option<LockSurface>,
}

impl SessionLockHandler for XrossState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        tracing::info!("session lock requested");
        self.switcher_cancel();
        if self.overview_open() {
            self.close_overview(None);
        }
        self.session_lock.pending = Some(confirmation);
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        self.refresh_pointer_focus();
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.session_lock = SessionLock::default();
        self.focus_top_window();
        self.refresh_pointer_focus();
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        let size = self.output_rect().size;
        surface.with_pending_state(|s| s.size = Some((size.w as u32, size.h as u32).into()));
        surface.send_configure();
        let target = KeyboardFocusTarget::Lock(surface.wl_surface().clone());
        self.session_lock.surface = Some(surface);
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, Some(target), SERIAL_COUNTER.next_serial());
        self.refresh_pointer_focus();
    }
}

impl XrossState {
    pub fn is_locked(&self) -> bool {
        self.session_lock.locked || self.session_lock.pending.is_some()
    }

    /// After a frame has been drawn: a pending lock is now safe to confirm.
    pub fn confirm_session_lock(&mut self) {
        if let Some(locker) = self.session_lock.pending.take() {
            locker.lock();
            self.session_lock.locked = true;
            tracing::info!("session locked");
        }
    }

    pub fn lock_surface(&self) -> Option<&WlSurface> {
        self.session_lock.surface.as_ref().map(|s| s.wl_surface()).filter(|s| s.is_alive())
    }
}

// --- Screen capture ---------------------------------------------------------------

/// A capture waiting for the next frame of `output`.
pub struct PendingCapture {
    pub target: CaptureTarget,
    pub output: Output,
    pub draw_cursor: bool,
}

/// Who asked: ext-image-copy-capture (grim) or wlr-screencopy (wf-recorder & co).
pub enum CaptureTarget {
    Ext(Frame),
    Wlr(super::wlr_screencopy::WlrCapture),
}

impl ImageCaptureSourceHandler for XrossState {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

impl OutputCaptureSourceHandler for XrossState {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}

impl ImageCopyCaptureHandler for XrossState {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        let output = source.user_data().get::<WeakOutput>()?.upgrade()?;
        let mode = output.current_mode()?;
        Some(BufferConstraints {
            size: (mode.size.w, mode.size.h).into(),
            shm: vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888],
            dma: None,
        })
    }

    fn new_session(&mut self, session: Session) {
        tracing::debug!(constraints = ?session.current_constraints().map(|c| c.size), "capture session");
        // Dropping the Session stops it, so keep it alive.
        self.capture_sessions.push(session);
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.capture_sessions.retain(|s| *s != session);
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        tracing::debug!(draw_cursor = session.draw_cursor(), "capture frame requested");
        // A locked screen is never captured.
        if self.is_locked() {
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
        let Some(output) = session.source().user_data().get::<WeakOutput>().and_then(|w| w.upgrade()) else {
            frame.fail(CaptureFailureReason::Stopped);
            return;
        };
        self.pending_captures.push(PendingCapture {
            target: CaptureTarget::Ext(frame),
            output,
            draw_cursor: session.draw_cursor(),
        });
    }
}
