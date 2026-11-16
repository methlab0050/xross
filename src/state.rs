use std::{collections::HashSet, ffi::OsString, sync::Arc};

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{Seat, SeatState, keyboard::Keycode, pointer::CursorImageStatus},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::{
            data_device::DataDeviceState, ext_data_control::DataControlState as ExtDataControlState,
            primary_selection::PrimarySelectionState, wlr_data_control::DataControlState as WlrDataControlState,
        },
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
        xwayland_shell::XWaylandShellState,
    },
    xwayland::X11Wm,
};

use crate::input::binds::Profile;

pub struct XrossState {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, XrossState>,
    pub loop_signal: LoopSignal,

    pub space: Space<Window>,
    /// Toplevels that exist but haven't committed a buffer yet.
    pub unmapped_windows: Vec<Window>,
    pub popups: PopupManager,

    // Protocol state
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<XrossState>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    /// Clipboard managers / wl-copy without focus (wlr and ext flavours).
    pub wlr_data_control_state: WlrDataControlState,
    pub ext_data_control_state: ExtDataControlState,
    pub xwayland_shell_state: XWaylandShellState,

    pub seat: Seat<Self>,
    pub cursor_status: CursorImageStatus,

    pub xwm: Option<X11Wm>,
    pub xdisplay: Option<u32>,
    /// Commands from `-c`, run once Xwayland is ready (so they get `DISPLAY`).
    pub pending_startup_commands: Vec<String>,

    pub bind_profile: Profile,
    /// Keycodes whose press was consumed by a bind; their release is swallowed too.
    pub suppressed_keys: HashSet<Keycode>,
    /// Pointer buttons whose press the compositor consumed (e.g. close button).
    pub suppressed_buttons: HashSet<u32>,
}

impl XrossState {
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>, bind_profile: Profile) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let wlr_data_control_state = WlrDataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| true);
        let ext_data_control_state = ExtDataControlState::new::<Self, _>(&dh, Some(&primary_selection_state), |_| true);
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop);

        Self {
            start_time: std::time::Instant::now(),
            socket_name,
            display_handle: dh,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),

            space: Space::default(),
            unmapped_windows: Vec::new(),
            popups: PopupManager::default(),

            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            layer_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            wlr_data_control_state,
            ext_data_control_state,
            xwayland_shell_state,

            seat,
            cursor_status: CursorImageStatus::default_named(),

            xwm: None,
            xdisplay: None,
            pending_startup_commands: Vec::new(),

            bind_profile,
            suppressed_keys: HashSet::new(),
            suppressed_buttons: HashSet::new(),
        }
    }

    fn init_wayland_listener(display: Display<XrossState>, event_loop: &mut EventLoop<Self>) -> OsString {
        let listening_socket = ListeningSocketSource::new_auto().unwrap();
        let socket_name = listening_socket.socket_name().to_os_string();
        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                if let Err(err) = state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(%err, "failed to insert client");
                }
            })
            .expect("Failed to init the wayland event source.");

        loop_handle
            .insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
                // Safety: we don't drop the display
                unsafe {
                    display.get_mut().dispatch_clients(state).unwrap();
                }
                Ok(PostAction::Continue)
            })
            .unwrap();

        socket_name
    }

    pub fn run_pending_startup_commands(&mut self) {
        for cmd in std::mem::take(&mut self.pending_startup_commands) {
            if let Err(err) = std::process::Command::new("sh").arg("-c").arg(&cmd).spawn() {
                tracing::warn!(%cmd, %err, "failed to spawn startup command");
            }
        }
    }
}

/// Data associated with a wayland client that connects to xross.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
