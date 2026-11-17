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
    /// The 2D workspace grid; each workspace owns its snap layout.
    pub workspaces: crate::layout::grid::Grid<crate::shell::workspace::Workspace>,
    pub next_ws_id: crate::shell::workspace::WsId,
    pub camera: crate::shell::camera::Camera,
    /// When the workspace minimap OSD hides (time since start).
    pub ws_osd_until: Option<std::time::Duration>,
    pub ws_osd: std::cell::RefCell<crate::render::OsdBuffers>,
    pub snap_combo: crate::input::combo::SnapCombo<Window>,
    /// Layout from before the current snap combo started (see [`crate::input::combo`]).
    pub snap_snapshot: Option<crate::layout::snap::Tiling<Window>>,
    /// Last seen usable area; snapped windows are re-laid out when it changes.
    pub last_usable_area: smithay::utils::Rectangle<i32, smithay::utils::Logical>,

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

    /// Set by the udev backend; `None` when nested.
    pub udev: Option<crate::backend::udev::SharedUdev>,
    /// Draw the cursor ourselves (no host compositor to do it).
    pub draw_cursor: bool,
    pub cursor: std::cell::RefCell<crate::cursor::CursorManager>,
    pub dmabuf_state: smithay::wayland::dmabuf::DmabufState,
    pub dmabuf_global: Option<smithay::wayland::dmabuf::DmabufGlobal>,

    pub bind_profile: Profile,
    /// Keycodes whose press was consumed by a bind; their release is swallowed too.
    pub suppressed_keys: HashSet<Keycode>,
    /// Pointer buttons whose press the compositor consumed (e.g. close button).
    pub suppressed_buttons: HashSet<u32>,
    /// What the touchpad swipe in progress drives.
    pub active_swipe: Option<crate::input::SwipeKind>,
    /// `xross msg screenshot PATH`: saved after the next frame (nested backend only).
    pub screenshot_request: Option<std::path::PathBuf>,
    /// Windows by recency of focus, most recent first (all workspaces).
    pub mru: Vec<Window>,
    pub switcher: Option<crate::layout::switcher::Session<Window>>,
    /// The switcher modifier was released inside the keyboard filter; commit after it.
    pub switcher_commit_pending: bool,
    pub switcher_ui: std::cell::RefCell<crate::shell::switcher::SwitcherUi>,
    pub overview: crate::shell::overview::Overview,
    pub overview_ui: std::cell::RefCell<crate::render::OverviewBuffers>,
    /// Detects the workspace modifier tapped alone (toggles the overview).
    pub mod_tap: crate::input::tap::ModTap,
    /// Overview actions deferred until after keyboard/pointer handling (they move focus).
    pub overview_toggle_pending: bool,
    pub overview_close_pending: Option<Option<crate::layout::grid::Coord>>,
    pub overview_focus_pending: Option<Window>,
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
            workspaces: crate::layout::grid::Grid::new(crate::shell::workspace::Workspace::new(0)),
            next_ws_id: 0,
            camera: Default::default(),
            ws_osd_until: None,
            ws_osd: Default::default(),
            snap_combo: Default::default(),
            snap_snapshot: None,
            last_usable_area: Default::default(),

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

            udev: None,
            draw_cursor: false,
            cursor: std::cell::RefCell::new(crate::cursor::CursorManager::new()),
            dmabuf_state: smithay::wayland::dmabuf::DmabufState::new(),
            dmabuf_global: None,

            bind_profile,
            suppressed_keys: HashSet::new(),
            suppressed_buttons: HashSet::new(),
            active_swipe: None,
            screenshot_request: None,
            mru: Vec::new(),
            switcher: None,
            switcher_commit_pending: false,
            switcher_ui: Default::default(),
            overview: Default::default(),
            overview_ui: Default::default(),
            mod_tap: Default::default(),
            overview_toggle_pending: false,
            overview_close_pending: None,
            overview_focus_pending: None,
        }
    }

    fn init_wayland_listener(display: Display<XrossState>, event_loop: &mut EventLoop<Self>) -> OsString {
        let listening_socket = ListeningSocketSource::new_auto().unwrap();
        let socket_name = listening_socket.socket_name().to_os_string();
        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                if let Err(err) = state.display_handle.insert_client(client_stream, Arc::new(ClientState::default())) {
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
