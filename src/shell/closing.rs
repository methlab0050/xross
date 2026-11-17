//! Close animations: when a window goes away, its last frame (title bar included) is
//! rendered into a texture while its surfaces still exist, and that snapshot fades and
//! shrinks out where the window was.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements, Id, Kind,
                surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
                texture::TextureRenderElement,
                utils::{CropRenderElement, RescaleRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    desktop::Window,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use super::{
    has_ssd,
    snap::open_spring,
    ssd::{self, DecorationStyle},
    workspace::{WsId, workspace_of},
};
use crate::{
    anim::spring::Animation,
    render::OutputRenderElement,
    shaders::{RoundedElement, Shaders},
    state::XrossState,
};

/// Closing windows shrink to this scale as they fade.
pub const CLOSE_SCALE: f64 = 0.92;

pub struct ClosingWindow {
    texture: GlesTexture,
    id: Id,
    /// Where the snapshot goes, in global coordinates on its workspace.
    pub rect: Rectangle<f64, Logical>,
    pub workspace: Option<WsId>,
    /// 0 -> 1 as it closes.
    anim: Animation,
}

impl ClosingWindow {
    pub fn progress(&self, now: Duration) -> f64 {
        self.anim.value(now).clamp(0.0, 1.0)
    }

    pub fn is_done(&self, now: Duration) -> bool {
        self.anim.is_done(now)
    }

    /// The snapshot drawn into `target` (output-local logical), faded by `alpha`.
    pub fn element(
        &self,
        renderer: &GlesRenderer,
        target: Rectangle<f64, Logical>,
        alpha: f32,
        scale: Scale<f64>,
    ) -> TextureRenderElement<GlesTexture> {
        let loc: Point<f64, Physical> = target.loc.to_physical(scale);
        // The whole snapshot, squeezed into `target` (without an explicit source, the
        // element would show only a target-sized corner of the texture).
        let tex = self.texture.size();
        let src = Rectangle::from_size(Size::<f64, Logical>::from((tex.w as f64, tex.h as f64)));
        TextureRenderElement::from_static_texture(
            self.id.clone(),
            renderer.context_id(),
            loc,
            self.texture.clone(),
            1,
            Transform::Normal,
            Some(alpha),
            Some(src),
            Some(target.size.to_i32_round()),
            None,
            Kind::Unspecified,
        )
    }
}

impl XrossState {
    /// Snapshot `window` before it disappears and start its close animation. Only
    /// windows that are on screen get one.
    pub fn start_close_animation(&mut self, window: &Window) {
        let now = self.start_time.elapsed();
        let Some(loc) = self.space.element_location(window) else {
            tracing::debug!("no close animation: window not mapped");
            return;
        };
        let workspace = workspace_of(window);
        // Menus and tooltips (X11 override-redirect) just vanish.
        let transient = window.x11_surface().is_some_and(|x| x.is_override_redirect());
        if transient || !self.is_visible(window) || self.is_locked() {
            return;
        }
        let scale = self.space.outputs().next().map_or(1.0, |o| o.current_scale().fractional_scale());

        // The area to capture, relative to the window's surface origin: its bounding box
        // plus our title bar.
        let geo = window.geometry();
        let mut area = window.bbox();
        let with_title = has_ssd(window) || ssd::drawn(window);
        if with_title {
            area = area.merge(ssd::title_rect(geo));
        }
        if area.size.w <= 0 || area.size.h <= 0 {
            tracing::debug!(?area, "no close animation: nothing to capture");
            return;
        }

        let rounded = self.rounded_with_title(window, with_title);
        let snapshot = self.with_renderer(|renderer| {
            let shaders = self.shaders(renderer);
            snapshot(renderer, shaders.as_ref(), window, geo, area, scale, with_title, rounded)
        });
        let Some(Ok(texture)) = snapshot else {
            if let Some(Err(err)) = snapshot {
                tracing::debug!(%err, "no close animation: snapshot failed");
            }
            return;
        };
        tracing::debug!(?area, "close animation");
        let origin = (loc - geo.loc + area.loc).to_f64();
        self.closing.push(ClosingWindow {
            texture,
            id: Id::new(),
            rect: Rectangle::new(origin, area.size.to_f64()),
            workspace,
            anim: Animation::new(0.0, 1.0, 0.0, open_spring(), now),
        });
    }

    pub fn tick_closing(&mut self) {
        let now = self.start_time.elapsed();
        self.closing.retain(|c| !c.is_done(now));
    }
}

/// Render the window (and its title bar) into a transparent texture covering `area`,
/// with rounded corners if `rounded`.
#[allow(clippy::too_many_arguments)]
fn snapshot(
    renderer: &mut GlesRenderer,
    shaders: Option<&Shaders>,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    area: Rectangle<i32, Logical>,
    scale: f64,
    with_title: bool,
    rounded: bool,
) -> Result<GlesTexture, String> {
    let scale_f = Scale::from(scale);
    let size: Size<i32, Physical> = area.size.to_physical_precise_round(scale);
    let surface_loc: Point<i32, Physical> = (Point::default() - area.loc).to_physical_precise_round(scale);
    let content = Rectangle::new(geo.loc - area.loc, geo.size);
    let mut frame = content;
    if with_title {
        frame = frame.merge(ssd::title_rect(content));
    }
    let radius_px = crate::config::current().appearance.corner_radius * scale;
    let mut elements: Vec<OutputRenderElement> = Vec::new();
    if with_title && let Some(shaders) = shaders {
        let style = DecorationStyle { focused: false, close_hovered: false, rounded };
        elements.extend(
            ssd::render_elements(renderer, shaders, window, content, Point::default(), scale_f, style, 1.0)
                .into_iter()
                .map(OutputRenderElement::PlainDecoration),
        );
    }
    let full = Rectangle::from_size(size);
    for e in window.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, surface_loc, scale_f, 1.0) {
        let tex_size = match e.texture() {
            WaylandSurfaceTexture::Texture(t) if rounded => Some(t.size()),
            _ => None,
        };
        match (tex_size, shaders) {
            (Some(tex_size), Some(s)) => {
                let e = RescaleRenderElement::from_element(e, Point::default(), 1.0);
                if let Some(e) = CropRenderElement::from_element(e, scale_f, full) {
                    elements.push(OutputRenderElement::RoundedWindow(RoundedElement::new(
                        e,
                        s.rounded_tex.clone(),
                        tex_size,
                        frame.to_f64().to_physical(scale),
                        radius_px,
                    )));
                }
            }
            _ => elements.push(OutputRenderElement::Surface(e)),
        }
    }
    if elements.is_empty() {
        return Err("nothing to draw".into());
    }

    let mut texture: GlesTexture =
        renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into()).map_err(|e| e.to_string())?;
    {
        let mut target = renderer.bind(&mut texture).map_err(|e| e.to_string())?;
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        tracker
            .render_output(renderer, &mut target, 0, &elements, [0.0, 0.0, 0.0, 0.0])
            .map_err(|e| format!("{e:?}"))?;
    }
    Ok(texture)
}
