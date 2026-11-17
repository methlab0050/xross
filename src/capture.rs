//! Screen capture for ext-image-copy-capture (grim, wf-recorder, ...): pending frames
//! are filled after the output renders, by drawing the same elements into an offscreen
//! texture and copying the result into the client's shm buffer.

use std::error::Error;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    output::Output,
    reexports::wayland_server::protocol::wl_shm,
    utils::{Buffer, Clock, Monotonic, Physical, Rectangle, Size, Transform},
    wayland::{image_copy_capture::CaptureFailureReason, shm::with_buffer_contents_mut},
};

use crate::{handlers::CaptureTarget, render::CLEAR_COLOR, state::XrossState};

/// Fill every capture waiting on `output`.
pub fn process(state: &mut XrossState, renderer: &mut GlesRenderer, output: &Output) {
    if state.pending_captures.iter().all(|c| &c.output != output) {
        return;
    }
    let (mine, rest): (Vec<_>, Vec<_>) =
        std::mem::take(&mut state.pending_captures).into_iter().partition(|c| &c.output == output);
    state.pending_captures = rest;

    for capture in mine {
        let image = render(state, renderer, output, capture.draw_cursor).map_err(|e| e.to_string());
        let now: std::time::Duration = Clock::<Monotonic>::new().now().into();
        match capture.target {
            CaptureTarget::Ext(frame) => {
                let full = Rectangle::from_size(image.as_ref().map(|i| i.size).unwrap_or_default());
                match image.and_then(|img| write_shm(&frame.buffer(), &img, full)) {
                    Ok(()) => frame.success(Transform::Normal, None, now),
                    Err(err) => {
                        tracing::warn!(%err, "capture failed");
                        frame.fail(CaptureFailureReason::Unknown);
                    }
                }
            }
            CaptureTarget::Wlr(wlr) => match image.and_then(|img| write_shm(&wlr.buffer, &img, wlr.region)) {
                Ok(()) => wlr.succeed(now),
                Err(err) => {
                    tracing::warn!(%err, "screencopy failed");
                    wlr.fail();
                }
            },
        }
    }
}

/// A captured frame: tightly packed RGBA rows, top to bottom.
struct Image {
    rgba: Vec<u8>,
    size: Size<i32, Physical>,
}

/// Render `output` offscreen.
fn render(
    state: &XrossState,
    renderer: &mut GlesRenderer,
    output: &Output,
    draw_cursor: bool,
) -> Result<Image, Box<dyn Error>> {
    let size = output.current_mode().ok_or("output has no mode")?.size;
    let scale = output.current_scale().fractional_scale();
    let elements = state.output_elements_for(renderer, output, draw_cursor);

    let buffer_size = Size::<i32, Buffer>::from((size.w, size.h));
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, buffer_size)?;
    let mapping = {
        let mut target = renderer.bind(&mut texture)?;
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        tracker.render_output(renderer, &mut target, 0, &elements, CLEAR_COLOR)?;
        renderer.copy_framebuffer(&target, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)?
    };
    let data = renderer.map_texture(&mapping)?;
    Ok(Image { rgba: data.to_vec(), size })
}

/// Copy `region` of the image into an ARGB/XRGB8888 shm buffer (little-endian B, G, R, A).
fn write_shm(
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    image: &Image,
    region: Rectangle<i32, Physical>,
) -> Result<(), String> {
    let size = region.size;
    if !Rectangle::from_size(image.size).contains_rect(region) {
        return Err("capture region outside the output".into());
    }
    let (rx, ry, iw) = (region.loc.x as usize, region.loc.y as usize, image.size.w as usize);
    with_buffer_contents_mut(buffer, |ptr, len, info| {
        if info.width != size.w || info.height != size.h {
            return Err(format!("buffer is {}x{}, output is {}x{}", info.width, info.height, size.w, size.h));
        }
        if !matches!(info.format, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888) {
            return Err(format!("unsupported format {:?}", info.format));
        }
        let (w, h, stride, offset) = (size.w as usize, size.h as usize, info.stride as usize, info.offset as usize);
        if offset + stride * h > len {
            return Err("buffer too small".into());
        }
        // Safety: bounds checked above; the pool is mapped for `len` bytes.
        let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset), stride * h) };
        for y in 0..h {
            // Offscreen textures read back top-down (unlike the window framebuffer).
            let src = &image.rgba[((ry + y) * iw + rx) * 4..][..w * 4];
            let row = &mut dst[y * stride..][..w * 4];
            for (d, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                *d = [s[2], s[1], s[0], s[3]];
            }
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?
}
