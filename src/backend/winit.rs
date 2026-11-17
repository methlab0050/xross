//! Nested backend: xross runs as a window inside the host compositor.

use std::time::Duration;

use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
};

use crate::state::XrossState;

pub fn init_winit(
    event_loop: &mut EventLoop<'static, XrossState>,
    state: &mut XrossState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (backend, winit) = winit::init::<GlesRenderer>()?;
    backend.window().set_title("xross (nested)");
    // Shared so protocol handlers can render too (window snapshots for close animations).
    let backend = std::rc::Rc::new(std::cell::RefCell::new(backend));
    state.winit = Some(backend.clone());
    let size = backend.borrow().window_size();

    let mode = Mode { size, refresh: 60_000 };
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "xross".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<XrossState>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        Some(smithay::output::Scale::Fractional(state.output_scale())),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop.handle().insert_source(winit, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            output.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
            layer_map_for_output(&output).arrange();
        }
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => {
            let mut backend = backend.borrow_mut();
            let age = backend.buffer_age().unwrap_or(0);
            let size = output.current_mode().unwrap().size;
            let mut screenshot = None;
            let render_result = {
                let (renderer, mut framebuffer) = match backend.bind() {
                    Ok(b) => b,
                    Err(err) => {
                        tracing::warn!(?err, "failed to bind winit backend");
                        return;
                    }
                };
                let elements = state.output_elements(renderer, &output);
                let res = damage_tracker
                    .render_output(renderer, &mut framebuffer, age, &elements, crate::config::current().background())
                    .map(|r| r.damage.cloned());
                if let Some(path) = state.screenshot_request.take() {
                    match crate::screenshot::start(renderer, &framebuffer, size) {
                        Ok(mapping) => screenshot = Some((mapping, path)),
                        Err(err) => tracing::warn!(?path, %err, "screenshot failed"),
                    }
                }
                res
            };
            let mut feedback = state.presentation_feedback(&output);
            match render_result {
                Ok(damage) => {
                    if let Err(err) = backend.submit(damage.as_deref()) {
                        tracing::warn!(?err, "failed to submit frame");
                    }
                    // No vblank info nested: call it presented now.
                    use smithay::{
                        reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind,
                        utils::{Clock, Monotonic},
                        wayland::presentation::Refresh,
                    };
                    let refresh = Duration::from_secs_f64(1000.0 / output.current_mode().unwrap().refresh as f64);
                    feedback.presented(Clock::<Monotonic>::new().now(), Refresh::fixed(refresh), 0, Kind::Vsync);
                }
                Err(err) => tracing::warn!(?err, "render failed"),
            }
            // Both of these leave the GL context without the window surface, so they
            // run after the frame is submitted.
            if let Some((mapping, path)) = screenshot {
                match crate::screenshot::finish(backend.renderer(), &mapping, size, &path) {
                    Ok(()) => tracing::info!(?path, "saved screenshot"),
                    Err(err) => tracing::warn!(?path, %err, "screenshot failed"),
                }
            }
            crate::capture::process(state, backend.renderer(), &output);

            state.post_render(&output);
            backend.window().request_redraw();
        }
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        _ => (),
    })?;

    Ok(())
}
