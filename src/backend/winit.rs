//! Nested backend: xross runs as a window inside the host compositor.

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

use crate::{render::CLEAR_COLOR, state::XrossState};

pub fn init_winit(
    event_loop: &mut EventLoop<'static, XrossState>,
    state: &mut XrossState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    backend.window().set_title("xross (nested)");

    let mode = Mode { size: backend.window_size(), refresh: 60_000 };
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
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
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
            let age = backend.buffer_age().unwrap_or(0);
            let render_result = {
                let (renderer, mut framebuffer) = match backend.bind() {
                    Ok(b) => b,
                    Err(err) => {
                        tracing::warn!(?err, "failed to bind winit backend");
                        return;
                    }
                };
                let elements = state.output_elements(renderer, &output);
                damage_tracker
                    .render_output(renderer, &mut framebuffer, age, &elements, CLEAR_COLOR)
                    .map(|r| r.damage.cloned())
            };
            match render_result {
                Ok(damage) => {
                    if let Err(err) = backend.submit(damage.as_deref()) {
                        tracing::warn!(?err, "failed to submit frame");
                    }
                }
                Err(err) => tracing::warn!(?err, "render failed"),
            }

            state.post_render(&output);
            backend.window().request_redraw();
        }
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        _ => (),
    })?;

    Ok(())
}
