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
    pub xdg_activation_state: smithay::wayland::xdg_activation::XdgActivationState,
    pub cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState,
    pub fractional_scale_state: smithay::wayland::fractional_scale::FractionalScaleManagerState,
    pub viewporter_state: smithay::wayland::viewporter::ViewporterState,
    pub presentation_state: smithay::wayland::presentation::PresentationState,
    pub idle_notifier_state: smithay::wayland::idle_notify::IdleNotifierState<Self>,
    pub idle_inhibit_state: smithay::wayland::idle_inhibit::IdleInhibitManagerState,
    /// Surfaces currently inhibiting idle (video players, presentations).
    pub idle_inhibitors: HashSet<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    pub session_lock: crate::handlers::SessionLock,
    pub image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState,
    pub output_capture_source_state: smithay::wayland::image_capture_source::OutputCaptureSourceState,
    pub image_copy_capture_state: smithay::wayland::image_copy_capture::ImageCopyCaptureState,
    /// Screen captures waiting for their output's next frame.
    pub pending_captures: Vec<crate::handlers::PendingCapture>,
    /// Snapshots of windows playing their close animation.
    pub closing: Vec<crate::shell::closing::ClosingWindow>,
    /// Custom shaders, compiled on first use.
    pub shaders: std::cell::RefCell<Option<crate::shaders::Shaders>>,
    /// Live capture sessions (dropping one stops it).
    pub capture_sessions: Vec<smithay::wayland::image_copy_capture::Session>,
    pub wlr_screencopy_state: crate::handlers::WlrScreencopyState,

    pub seat: Seat<Self>,
    pub cursor_status: CursorImageStatus,

    pub xwm: Option<X11Wm>,
    pub xdisplay: Option<u32>,
    /// The Xwayland client, for its scale (`xwayland.native_scaling`).
    pub xwayland_client: Option<smithay::reexports::wayland_server::Client>,
    /// The `Xft.dpi` last given to X11 apps.
    pub x_resources_dpi: Option<i32>,
    /// Commands from `-c`, run once Xwayland is ready (so they get `DISPLAY`).
    pub pending_startup_commands: Vec<String>,

    /// Set by the udev backend; `None` when nested.
    pub udev: Option<crate::backend::udev::SharedUdev>,
    /// Set by the winit backend; `None` on a TTY.
    pub winit: Option<
        std::rc::Rc<
            std::cell::RefCell<
                smithay::backend::winit::WinitGraphicsBackend<smithay::backend::renderer::gles::GlesRenderer>,
            >,
        >,
    >,
    /// Draw the cursor ourselves (no host compositor to do it).
    pub draw_cursor: bool,
    pub cursor: std::cell::RefCell<crate::cursor::CursorManager>,
    pub dmabuf_state: smithay::wayland::dmabuf::DmabufState,
    pub dmabuf_global: Option<smithay::wayland::dmabuf::DmabufGlobal>,

    pub bind_profile: Profile,
    /// The profile when neither `--binds` nor the config chooses one (by backend).
    pub default_profile: Profile,
    /// `--binds` on the command line: wins over the config.
    pub binds_override: Option<Profile>,
    /// `--scale` on the command line: wins over the config.
    pub scale_override: Option<f64>,
    /// Compiled `[binds]` from the config.
    pub user_binds: Vec<crate::input::user_binds::UserBind>,
    pub config_path: Option<std::path::PathBuf>,
    pub config_stamp: Option<(std::time::SystemTime, u64)>,
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
    /// A tap bind to run after the keyboard filter.
    pub pending_tap: Option<crate::input::user_binds::BoundAction>,
    /// Keys typed in the overview, held back for the launcher it started.
    pub launch_typing: Option<crate::input::launch::LaunchTyping>,
    pub launch_seq: u64,
    /// The launcher, started hidden while the overview is up.
    pub prewarm: Option<crate::input::launch::Prewarm>,
    pub prewarm_attempted: bool,
    /// Processes we started, reaped once they exit.
    pub children: Vec<std::process::Child>,
    /// Modifiers of the bind that opened the switcher; releasing them commits.
    pub switcher_hold: Option<crate::input::binds::Mods>,
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
        use smithay::wayland::{
            cursor_shape::CursorShapeManagerState,
            fractional_scale::FractionalScaleManagerState,
            idle_inhibit::IdleInhibitManagerState,
            idle_notify::IdleNotifierState,
            image_capture_source::{ImageCaptureSourceState, OutputCaptureSourceState},
            image_copy_capture::ImageCopyCaptureState,
            presentation::PresentationState,
            session_lock::SessionLockManagerState,
            viewporter::ViewporterState,
            xdg_activation::XdgActivationState,
        };
        let monotonic = smithay::utils::Clock::<smithay::utils::Monotonic>::new().id() as u32;

        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop);

        Self {
            start_time: std::time::Instant::now(),
            socket_name,
            display_handle: dh.clone(),
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
            xdg_activation_state: XdgActivationState::new::<Self>(&dh),
            cursor_shape_state: CursorShapeManagerState::new::<Self>(&dh),
            fractional_scale_state: FractionalScaleManagerState::new::<Self>(&dh),
            viewporter_state: ViewporterState::new::<Self>(&dh),
            presentation_state: PresentationState::new::<Self>(&dh, monotonic),
            idle_notifier_state: IdleNotifierState::new(&dh, event_loop.handle()),
            idle_inhibit_state: IdleInhibitManagerState::new::<Self>(&dh),
            idle_inhibitors: HashSet::new(),
            session_lock_state: SessionLockManagerState::new::<Self, _>(&dh, |_| true),
            session_lock: Default::default(),
            image_capture_source_state: ImageCaptureSourceState::new(),
            output_capture_source_state: OutputCaptureSourceState::new::<Self>(&dh),
            image_copy_capture_state: ImageCopyCaptureState::new::<Self>(&dh),
            pending_captures: Vec::new(),
            closing: Vec::new(),
            shaders: Default::default(),
            capture_sessions: Vec::new(),
            wlr_screencopy_state: crate::handlers::WlrScreencopyState::new(&dh),

            seat,
            cursor_status: CursorImageStatus::default_named(),

            xwm: None,
            xdisplay: None,
            xwayland_client: None,
            x_resources_dpi: None,
            pending_startup_commands: Vec::new(),

            udev: None,
            winit: None,
            draw_cursor: false,
            cursor: std::cell::RefCell::new(crate::cursor::CursorManager::new()),
            dmabuf_state: smithay::wayland::dmabuf::DmabufState::new(),
            dmabuf_global: None,

            bind_profile,
            default_profile: bind_profile,
            binds_override: None,
            scale_override: None,
            user_binds: Vec::new(),
            config_path: None,
            config_stamp: None,
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
            pending_tap: None,
            launch_typing: None,
            launch_seq: 0,
            prewarm: None,
            prewarm_attempted: false,
            children: Vec::new(),
            switcher_hold: None,
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
            self.spawn(&cmd);
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
