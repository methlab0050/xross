//! wlr-screencopy (v3), for recorders that don't speak ext-image-copy-capture yet
//! (wf-recorder, older grim). Smithay doesn't implement it, so this is the protocol
//! glue; the pixels come from the same capture path as ext-image-copy-capture
//! (`crate::capture`). Only shm buffers are offered.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::GlobalId,
            protocol::{wl_buffer::WlBuffer, wl_shm},
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Size},
};

use super::{CaptureTarget, PendingCapture};
use crate::state::XrossState;

const VERSION: u32 = 3;

pub struct WlrScreencopyState {
    _global: GlobalId,
}

impl WlrScreencopyState {
    pub fn new(dh: &DisplayHandle) -> Self {
        Self { _global: dh.create_global::<XrossState, ZwlrScreencopyManagerV1, _>(VERSION, ()) }
    }
}

/// Per-frame data: what to capture.
pub struct FrameData {
    output: Output,
    /// The captured area, in output-local physical pixels.
    region: Rectangle<i32, Physical>,
    overlay_cursor: bool,
    used: AtomicBool,
}

/// A wlr frame waiting for its output's next render.
pub struct WlrCapture {
    pub frame: ZwlrScreencopyFrameV1,
    pub buffer: WlBuffer,
    pub region: Rectangle<i32, Physical>,
    pub with_damage: bool,
}

impl WlrCapture {
    pub fn succeed(self, presented: std::time::Duration) {
        self.frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
        if self.with_damage {
            self.frame.damage(0, 0, self.region.size.w as u32, self.region.size.h as u32);
        }
        let secs = presented.as_secs();
        self.frame.ready((secs >> 32) as u32, (secs & 0xffff_ffff) as u32, presented.subsec_nanos());
    }

    pub fn fail(self) {
        self.frame.failed();
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for XrossState {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for XrossState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_manager_v1::Request;
        let (frame, overlay_cursor, wl_output, logical_region) = match request {
            Request::CaptureOutput { frame, overlay_cursor, output } => (frame, overlay_cursor, output, None),
            Request::CaptureOutputRegion { frame, overlay_cursor, output, x, y, width, height } => (
                frame,
                overlay_cursor,
                output,
                Some(Rectangle::<i32, Logical>::new((x, y).into(), (width, height).into())),
            ),
            Request::Destroy => return,
            _ => return,
        };

        let output = Output::from_resource(&wl_output);
        let Some((output, mode)) = output.and_then(|o| o.current_mode().map(|m| (o, m))) else {
            let frame = data_init.init(frame, None::<FrameData>);
            frame.failed();
            return;
        };
        let full = Rectangle::<i32, Physical>::from_size(mode.size);
        let region = match logical_region {
            None => full,
            Some(r) => {
                let scale = output.current_scale().fractional_scale();
                let r = r.to_f64().to_physical_precise_round(scale);
                r.intersection(full).unwrap_or_else(|| Rectangle::new(Point::default(), Size::from((1, 1))))
            }
        };
        let frame = data_init
            .init(frame, Some(FrameData { output, region, overlay_cursor: overlay_cursor != 0, used: false.into() }));

        let (w, h) = (region.size.w as u32, region.size.h as u32);
        frame.buffer(wl_shm::Format::Xrgb8888, w, h, w * 4);
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Option<FrameData>> for XrossState {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &Option<FrameData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_frame_v1::Request;
        let (buffer, with_damage) = match request {
            Request::Copy { buffer } => (buffer, false),
            Request::CopyWithDamage { buffer } => (buffer, true),
            Request::Destroy => return,
            _ => return,
        };
        let Some(data) = data else {
            frame.failed();
            return;
        };
        if data.used.swap(true, Ordering::SeqCst) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::AlreadyUsed, "frame already used");
            return;
        }
        // A locked screen is never captured.
        if state.is_locked() {
            frame.failed();
            return;
        }
        state.pending_captures.push(PendingCapture {
            target: CaptureTarget::Wlr(WlrCapture { frame: frame.clone(), buffer, region: data.region, with_damage }),
            output: data.output.clone(),
            draw_cursor: data.overlay_cursor,
        });
    }
}
