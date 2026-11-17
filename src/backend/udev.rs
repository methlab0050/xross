//! Standalone backend: DRM/KMS + GBM + EGL for output, libinput for input, libseat for
//! session/VT management. Run from a TTY.
//!
//! Scope for now: the primary GPU only, and one output (the first connected connector).

use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

use smithay::{
    backend::{
        SwapBuffersError,
        allocator::{
            Fourcc,
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmAccessError, DrmDevice, DrmDeviceFd, DrmError, DrmEvent, DrmNode, NodeType,
            compositor::FrameFlags,
            exporter::gbm::{GbmFramebufferExporter, NodeFilter},
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, ImportMemWl, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, primary_gpu},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Scale as OutputScale},
    reexports::{
        calloop::{
            EventLoop, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{ModeTypeFlags, connector, crtc},
        input::{DeviceCapability, Libinput},
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::DeviceFd,
    wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};
use smithay_drm_extras::{
    display_info,
    drm_scanner::{DrmScanEvent, DrmScanner},
};

use crate::state::XrossState;

// 8-bit formats only: widely supported and fine for an SDR desktop.
const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Abgr8888, Fourcc::Argb8888];

/// Each queued frame carries the presentation-time feedback for what it shows.
type FrameData = Option<smithay::desktop::utils::OutputPresentationFeedback>;
type GbmDrmOutput = DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, FrameData, DrmDeviceFd>;
type GbmDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, FrameData, DrmDeviceFd>;

pub struct UdevData {
    pub session: LibSeatSession,
    primary_gpu: DrmNode,
    pub(crate) renderer: Option<GlesRenderer>,
    gpu: Option<Gpu>,
    /// A render timer is armed; avoids piling up redundant timers.
    render_scheduled: bool,
    /// Input devices, so config changes can be reapplied.
    input_devices: Vec<smithay::reexports::input::Device>,
}

pub type SharedUdev = Rc<RefCell<UdevData>>;

struct Gpu {
    node: DrmNode,
    manager: GbmDrmOutputManager,
    scanner: DrmScanner,
    surface: Option<OutputSurface>,
    _token: RegistrationToken,
}

struct OutputSurface {
    crtc: crtc::Handle,
    output: Output,
    global: GlobalId,
    drm_output: GbmDrmOutput,
    /// A repaint is already scheduled (vblank pending or timer armed).
    frame_pending: bool,
}

impl UdevData {
    /// Reapply touchpad settings to every input device (after a config change).
    pub fn reconfigure_input(&mut self, touchpad: &crate::config::Touchpad) {
        for device in &mut self.input_devices {
            configure_input_device(device, touchpad);
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(?err, vt, "failed to switch VT");
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.renderer.as_mut().is_some_and(|r| r.import_dmabuf(dmabuf, None).is_ok())
    }
}

pub fn init_udev(
    event_loop: &mut EventLoop<'static, XrossState>,
    state: &mut XrossState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (session, notifier) = LibSeatSession::new().map_err(|e| format!("could not start a libseat session: {e}"))?;
    let seat_name = session.seat();

    let primary_gpu = match std::env::var("XROSS_DRM_DEVICE") {
        Ok(path) => DrmNode::from_path(path)?,
        Err(_) => primary_gpu(&seat_name)?.and_then(|p| DrmNode::from_path(p).ok()).ok_or("no GPU found")?,
    };
    let primary_gpu = primary_gpu.node_with_type(NodeType::Primary).and_then(Result::ok).unwrap_or(primary_gpu);
    tracing::info!(%primary_gpu, "using primary GPU");

    let udev = Rc::new(RefCell::new(UdevData {
        session: session.clone(),
        primary_gpu,
        renderer: None,
        gpu: None,
        render_scheduled: false,
        input_devices: Vec::new(),
    }));
    state.udev = Some(udev.clone());
    state.draw_cursor = true;

    // Input.
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat_name).map_err(|()| "failed to assign libinput seat")?;
    let libinput_backend = LibinputInputBackend::new(libinput.clone());
    let input_udev = udev.clone();
    event_loop.handle().insert_source(libinput_backend, move |mut event, _, state| {
        match &mut event {
            InputEvent::DeviceAdded { device } => {
                configure_input_device(device, &crate::config::current().touchpad);
                input_udev.borrow_mut().input_devices.push(device.clone());
            }
            InputEvent::DeviceRemoved { device } => input_udev.borrow_mut().input_devices.retain(|d| d != device),
            _ => {}
        }
        state.process_input_event(event);
    })?;

    // Session (VT switches).
    let session_udev = udev.clone();
    event_loop.handle().insert_source(notifier, move |event, _, state| match event {
        SessionEvent::PauseSession => {
            tracing::info!("session paused");
            libinput.suspend();
            if let Some(gpu) = session_udev.borrow_mut().gpu.as_mut() {
                gpu.manager.pause();
            }
        }
        SessionEvent::ActivateSession => {
            tracing::info!("session resumed");
            if let Err(err) = libinput.resume() {
                tracing::error!(?err, "failed to resume libinput");
            }
            if let Some(gpu) = session_udev.borrow_mut().gpu.as_mut() {
                if let Err(err) = gpu.manager.lock().activate(false) {
                    tracing::error!(?err, "failed to reactivate DRM device");
                }
                if let Some(surface) = gpu.surface.as_mut() {
                    surface.frame_pending = false;
                }
            }
            schedule_render(state, Duration::ZERO);
        }
    })?;

    // GPUs.
    let udev_backend = UdevBackend::new(&seat_name)?;
    for (device_id, path) in udev_backend.device_list() {
        if let Ok(node) = DrmNode::from_dev_id(device_id)
            && node == primary_gpu
        {
            device_added(state, &udev, node, path)?;
        }
    }
    let hotplug_udev = udev.clone();
    event_loop.handle().insert_source(udev_backend, move |event, _, state| match event {
        UdevEvent::Added { device_id, path } => {
            if let Ok(node) = DrmNode::from_dev_id(device_id)
                && node == hotplug_udev.borrow().primary_gpu
                && hotplug_udev.borrow().gpu.is_none()
                && let Err(err) = device_added(state, &hotplug_udev, node, &path)
            {
                tracing::error!(%err, "failed to add GPU");
            }
        }
        UdevEvent::Changed { device_id } => {
            if hotplug_udev.borrow().gpu.as_ref().is_some_and(|g| g.node.dev_id() == device_id) {
                scan_connectors(state, &hotplug_udev);
            }
        }
        UdevEvent::Removed { device_id } => {
            if hotplug_udev.borrow().gpu.as_ref().is_some_and(|g| g.node.dev_id() == device_id) {
                tracing::warn!("primary GPU removed");
                let gpu = hotplug_udev.borrow_mut().gpu.take();
                if let Some(surface) = gpu.and_then(|g| g.surface) {
                    remove_output(state, surface);
                }
            }
        }
    })?;

    // Buffer formats for clients.
    let mut udev_ref = udev.borrow_mut();
    let renderer = udev_ref.renderer.as_mut().ok_or("primary GPU could not be initialized")?;
    state.shm_state.update_formats(renderer.shm_formats());
    let feedback = DmabufFeedbackBuilder::new(primary_gpu.dev_id(), renderer.dmabuf_formats()).build()?;
    let global = state.dmabuf_state.create_global_with_default_feedback::<XrossState>(&state.display_handle, &feedback);
    state.dmabuf_global = Some(global);

    Ok(())
}

fn configure_input_device(device: &mut smithay::reexports::input::Device, touchpad: &crate::config::Touchpad) {
    let is_touchpad = device.config_tap_finger_count() > 0;
    if is_touchpad {
        let _ = device.config_tap_set_enabled(touchpad.tap);
    }
    if device.has_capability(DeviceCapability::Pointer) && device.config_scroll_has_natural_scroll() {
        // Natural scrolling (content follows fingers) is for touchpads; mice keep wheels as-is.
        let _ = device.config_scroll_set_natural_scroll_enabled(is_touchpad && touchpad.natural_scroll);
    }
}

fn device_added(
    state: &mut XrossState,
    udev: &SharedUdev,
    node: DrmNode,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let fd =
        udev.borrow_mut().session.open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd)?;

    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&display)?;
    let renderer = unsafe { GlesRenderer::new(context)? };
    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();

    let token = state.loop_handle.insert_source(notifier, |event, metadata, state| match event {
        DrmEvent::VBlank(_crtc) => on_vblank(state, metadata),
        DrmEvent::Error(err) => tracing::error!(?err, "DRM error"),
    })?;

    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::Node(node));
    let manager =
        DrmOutputManager::new(drm, allocator, exporter, Some(gbm), COLOR_FORMATS.iter().copied(), render_formats);

    {
        let mut u = udev.borrow_mut();
        u.renderer = Some(renderer);
        u.gpu = Some(Gpu { node, manager, scanner: DrmScanner::new(), surface: None, _token: token });
    }
    scan_connectors(state, udev);
    Ok(())
}

fn scan_connectors(state: &mut XrossState, udev: &SharedUdev) {
    let events = {
        let mut u = udev.borrow_mut();
        let Some(gpu) = u.gpu.as_mut() else { return };
        match gpu.scanner.scan_connectors(gpu.manager.device()) {
            Ok(result) => result.into_iter().collect::<Vec<_>>(),
            Err(err) => {
                tracing::warn!(?err, "failed to scan connectors");
                return;
            }
        }
    };

    for event in events {
        match event {
            DrmScanEvent::Connected { connector, crtc: Some(crtc) } => {
                connector_connected(state, udev, connector, crtc)
            }
            DrmScanEvent::Disconnected { crtc: Some(crtc), .. } => {
                let surface = {
                    let mut u = udev.borrow_mut();
                    let gpu = u.gpu.as_mut().unwrap();
                    if gpu.surface.as_ref().is_some_and(|s| s.crtc == crtc) { gpu.surface.take() } else { None }
                };
                if let Some(surface) = surface {
                    remove_output(state, surface);
                    // Bring up another connected display, if any.
                    let remaining: Vec<_> = {
                        let u = udev.borrow();
                        u.gpu.as_ref().unwrap().scanner.crtcs().map(|(c, h)| (c.clone(), h)).collect()
                    };
                    if let Some((connector, crtc)) = remaining.into_iter().next() {
                        connector_connected(state, udev, connector, crtc);
                    }
                }
            }
            _ => {}
        }
    }
}

fn connector_connected(state: &mut XrossState, udev: &SharedUdev, connector: connector::Info, crtc: crtc::Handle) {
    if setup_output(state, udev, connector, crtc) {
        schedule_render(state, Duration::ZERO);
    }
}

/// Returns true if an output was created.
fn setup_output(state: &mut XrossState, udev: &SharedUdev, connector: connector::Info, crtc: crtc::Handle) -> bool {
    let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
    let mut guard = udev.borrow_mut();
    let u = &mut *guard;
    let (Some(gpu), Some(renderer)) = (u.gpu.as_mut(), u.renderer.as_mut()) else {
        return false;
    };
    if gpu.surface.is_some() {
        tracing::info!(%name, "ignoring additional output (single-output for now)");
        return false;
    }

    let Some(drm_mode) = connector
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| connector.modes().first())
        .copied()
    else {
        tracing::warn!(%name, "connector has no modes");
        return false;
    };
    let wl_mode = WlMode::from(drm_mode);

    let info = display_info::for_connector(gpu.manager.device(), connector.handle());
    let (w, h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (w as i32, h as i32).into(),
            subpixel: connector.subpixel().into(),
            make: info.as_ref().and_then(|i| i.make()).unwrap_or_else(|| "Unknown".into()),
            model: info.as_ref().and_then(|i| i.model()).unwrap_or_else(|| "Unknown".into()),
            serial_number: info.as_ref().and_then(|i| i.serial()).unwrap_or_else(|| "Unknown".into()),
        },
    );
    let global = output.create_global::<XrossState>(&state.display_handle);
    output.set_preferred(wl_mode);
    // Scale from --scale / the config (see `XrossState::output_scale`).
    output.change_current_state(
        Some(wl_mode),
        None,
        Some(OutputScale::Fractional(state.output_scale())),
        Some((0, 0).into()),
    );

    let planes = gpu.manager.device().planes(&crtc).ok();
    let drm_output = match gpu.manager.lock().initialize_output::<_, crate::render::OutputRenderElement>(
        crtc,
        drm_mode,
        &[connector.handle()],
        &output,
        planes,
        renderer,
        &DrmOutputRenderElements::default(),
    ) {
        Ok(o) => o,
        Err(err) => {
            tracing::error!(%name, ?err, "failed to initialize output");
            state.display_handle.remove_global::<XrossState>(global);
            return false;
        }
    };

    tracing::info!(%name, mode = ?wl_mode, "output connected");
    state.space.map_output(&output, (0, 0));
    smithay::desktop::layer_map_for_output(&output).arrange();
    gpu.surface = Some(OutputSurface { crtc, output, global, drm_output, frame_pending: false });
    true
}

fn remove_output(state: &mut XrossState, surface: OutputSurface) {
    tracing::info!(name = surface.output.name(), "output disconnected");
    state.space.unmap_output(&surface.output);
    surface.output.leave_all();
    state.display_handle.remove_global::<XrossState>(surface.global);
}

fn schedule_render(state: &XrossState, delay: Duration) {
    let Some(udev) = state.udev.as_ref() else {
        return;
    };
    {
        let mut u = udev.borrow_mut();
        if u.render_scheduled {
            return;
        }
        u.render_scheduled = true;
    }
    let _ = state.loop_handle.insert_source(Timer::from_duration(delay), |_, _, state| {
        if let Some(udev) = state.udev.as_ref() {
            udev.borrow_mut().render_scheduled = false;
        }
        render(state);
        TimeoutAction::Drop
    });
}

fn on_vblank(state: &mut XrossState, metadata: &mut Option<smithay::backend::drm::DrmEventMetadata>) {
    let Some(udev) = state.udev.clone() else {
        return;
    };
    let refresh = {
        let mut u = udev.borrow_mut();
        let Some(surface) = u.gpu.as_mut().and_then(|g| g.surface.as_mut()) else {
            return;
        };
        let refresh = surface.output.current_mode().map(|m| m.refresh).unwrap_or(60_000);
        match surface.drm_output.frame_submitted() {
            Ok(Some(Some(mut feedback))) => {
                use smithay::{
                    backend::drm::DrmEventTime,
                    reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind,
                    wayland::presentation::Refresh,
                };
                let frame = Duration::from_secs_f64(1000.0 / refresh as f64);
                // Prefer the kernel's vblank timestamp; fall back to now.
                let hw_time = metadata.as_ref().and_then(|m| match m.time {
                    DrmEventTime::Monotonic(t) if !t.is_zero() => Some(t),
                    _ => None,
                });
                let seq = metadata.as_ref().map_or(0, |m| m.sequence as u64);
                let (time, flags) = match hw_time {
                    Some(t) => (t, Kind::Vsync | Kind::HwClock | Kind::HwCompletion),
                    None => (smithay::utils::Clock::<smithay::utils::Monotonic>::new().now().into(), Kind::Vsync),
                };
                feedback.presented::<_, smithay::utils::Monotonic>(time, Refresh::fixed(frame), seq, flags);
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "frame_submitted failed"),
        }
        surface.frame_pending = false;
        refresh
    };
    // Repaint ~60% into the frame: gives clients time to submit buffers for this vblank
    // while leaving the compositor enough time to render (see anvil's rationale).
    let frame = Duration::from_secs_f64(1000.0 / refresh as f64);
    schedule_render(state, frame.mul_f64(0.6));
}

fn render(state: &mut XrossState) {
    let Some(udev) = state.udev.clone() else {
        return;
    };
    let mut guard = udev.borrow_mut();
    let u = &mut *guard;
    if !u.session.is_active() {
        return;
    }
    let (Some(gpu), Some(renderer)) = (u.gpu.as_mut(), u.renderer.as_mut()) else {
        return;
    };
    let Some(surface) = gpu.surface.as_mut() else {
        return;
    };
    if surface.frame_pending {
        return;
    }
    let output = surface.output.clone();

    let elements = state.output_elements(renderer, &output);
    let result = surface
        .drm_output
        .render_frame(renderer, &elements, crate::config::current().background(), FrameFlags::DEFAULT)
        .map(|r| !r.is_empty)
        .map_err(|err| match err {
            smithay::backend::drm::compositor::RenderFrameError::PrepareFrame(e) => SwapBuffersError::from(e),
            smithay::backend::drm::compositor::RenderFrameError::RenderFrame(
                smithay::backend::renderer::damage::Error::Rendering(e),
            ) => SwapBuffersError::from(e),
            other => SwapBuffersError::TemporaryFailure(format!("{other:?}").into()),
        });

    let queued = match result {
        Ok(true) => match surface.drm_output.queue_frame(Some(state.presentation_feedback(&output))) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(?err, "failed to queue frame");
                false
            }
        },
        Ok(false) => false,
        Err(err) => {
            if let SwapBuffersError::ContextLost(e) = &err
                && matches!(e.downcast_ref::<DrmError>(), Some(DrmError::TestFailed(_)))
            {
                // Usually after a VT switch where another DRM master changed the CRTC setup.
                let _ = gpu.manager.device_mut().reset_state();
            } else if !matches!(&err, SwapBuffersError::TemporaryFailure(e)
                if matches!(e.downcast_ref::<DrmError>(), Some(DrmError::DeviceInactive | DrmError::Access(DrmAccessError { .. }))))
            {
                tracing::warn!(?err, "render failed");
            }
            false
        }
    };
    surface.frame_pending = queued;
    let refresh = output.current_mode().map(|m| m.refresh).unwrap_or(60_000);
    drop(elements);
    crate::capture::process(state, renderer, &output);
    drop(guard);

    state.post_render(&output);
    if !queued {
        // Nothing changed (or a temporary failure): poll again in about a frame.
        schedule_render(state, Duration::from_secs_f64(1000.0 / refresh as f64));
    }
}

impl DmabufHandler for XrossState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let ok = self.udev.as_ref().is_some_and(|u| u.borrow_mut().import_dmabuf(&dmabuf));
        if ok {
            let _ = notifier.successful::<XrossState>();
        } else {
            notifier.failed();
        }
    }
}
