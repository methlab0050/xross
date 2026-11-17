//! Custom GLES shaders: rounded window corners, soft shadows, and rounded fills (title
//! bars, buttons), plus [`RoundedElement`], which clips any texture element to a
//! rounded rectangle.

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{
            GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
        },
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
};

/// Texture shader that fades out everything outside `clip_rect` (rounded by `radius`).
/// `v_coords * coord_scale + coord_offset` maps texture coordinates to output pixels.
const ROUNDED_TEX: &str = r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 coord_scale;
uniform vec2 coord_offset;
uniform vec4 clip_rect;
uniform float radius;

float coverage(vec2 pos) {
    vec2 half_size = clip_rect.zw * 0.5;
    vec2 q = abs(pos - (clip_rect.xy + half_size)) - (half_size - vec2(radius));
    float dist = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
    return clamp(0.5 - dist, 0.0, 1.0);
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    color *= coverage(v_coords * coord_scale + coord_offset);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

/// A soft shadow around `rect` (within the element), never under it: a Gaussian
/// falloff that fades to nothing over `softness`.
const SHADOW: &str = r#"
precision highp float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 shadow_color;
uniform vec4 rect;
uniform float radius;
uniform float softness;

void main() {
    vec2 pos = v_coords * size;
    vec2 half_size = rect.zw * 0.5;
    vec2 q = abs(pos - (rect.xy + half_size)) - (half_size - vec2(radius));
    float dist = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
    float sigma = softness / 3.0;
    float a = dist < 0.0 ? 0.0 : exp(-(dist * dist) / (2.0 * sigma * sigma));
    // Reach exactly zero at the element's edge, so its boundary never shows.
    float edge = min(min(pos.x, pos.y), min(size.x - pos.x, size.y - pos.y));
    a *= smoothstep(0.0, softness * 0.35, edge);
    vec4 color = vec4(shadow_color.rgb * shadow_color.a, shadow_color.a) * a * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

/// A filled rectangle with per-corner radii (top-left, top-right, bottom-right,
/// bottom-left), anti-aliased. `extend` pretends the shape continues that many pixels
/// below the drawn area (a title bar is the top of a taller rounded window).
const ROUNDED_RECT: &str = r#"
precision highp float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec4 color;
uniform vec4 radii;
uniform float extend;

void main() {
    vec2 shape = size + vec2(0.0, extend);
    vec2 p = v_coords * size - shape * 0.5;
    float r = p.x < 0.0 ? (p.y < 0.0 ? radii.x : radii.w) : (p.y < 0.0 ? radii.y : radii.z);
    r = min(r, min(shape.x, shape.y) * 0.5);
    vec2 q = abs(p) - (shape * 0.5 - vec2(r));
    float dist = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
    float a = clamp(0.5 - dist, 0.0, 1.0);
    vec4 c = vec4(color.rgb * color.a, color.a) * a * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        c = vec4(0.0, 0.2, 0.0, 0.2) + c * 0.8;
#endif
    gl_FragColor = c;
}
"#;

#[derive(Debug, Clone)]
pub struct Shaders {
    pub rounded_tex: GlesTexProgram,
    pub shadow: GlesPixelProgram,
    pub rounded_rect: GlesPixelProgram,
}

impl Shaders {
    pub fn compile(renderer: &mut GlesRenderer) -> Result<Self, GlesError> {
        let name = |n: &'static str, t| UniformName { name: n.into(), type_: t };
        Ok(Self {
            rounded_tex: renderer.compile_custom_texture_shader(
                ROUNDED_TEX,
                &[
                    name("coord_scale", UniformType::_2f),
                    name("coord_offset", UniformType::_2f),
                    name("clip_rect", UniformType::_4f),
                    name("radius", UniformType::_1f),
                ],
            )?,
            shadow: renderer.compile_custom_pixel_shader(
                SHADOW,
                &[
                    name("shadow_color", UniformType::_4f),
                    name("rect", UniformType::_4f),
                    name("radius", UniformType::_1f),
                    name("softness", UniformType::_1f),
                ],
            )?,
            rounded_rect: renderer.compile_custom_pixel_shader(
                ROUNDED_RECT,
                &[name("color", UniformType::_4f), name("radii", UniformType::_4f), name("extend", UniformType::_1f)],
            )?,
        })
    }
}

/// Wraps a texture element so it's drawn clipped to a rounded rectangle.
#[derive(Debug)]
pub struct RoundedElement<E> {
    inner: E,
    program: GlesTexProgram,
    /// Texture size of the wrapped surface (buffer pixels).
    tex_size: Size<f64, Buffer>,
    /// Clip rectangle and radius, in output pixels.
    clip: Rectangle<f64, Physical>,
    radius: f64,
}

impl<E> RoundedElement<E> {
    pub fn new(
        inner: E,
        program: GlesTexProgram,
        tex_size: Size<i32, Buffer>,
        clip: Rectangle<f64, Physical>,
        radius: f64,
    ) -> Self {
        Self { inner, program, tex_size: tex_size.to_f64(), clip, radius }
    }
}

impl<E: Element> Element for RoundedElement<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    /// The corners aren't opaque any more; claim nothing rather than work out the shape.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for RoundedElement<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        // Texture coordinates -> output pixels: v * tex_size is in buffer pixels, which
        // map linearly from `src` onto `dst`.
        let (sx, sy) = (dst.size.w as f64 / src.size.w.max(1e-6), dst.size.h as f64 / src.size.h.max(1e-6));
        let coord_scale = ((self.tex_size.w * sx) as f32, (self.tex_size.h * sy) as f32);
        let coord_offset = ((dst.loc.x as f64 - src.loc.x * sx) as f32, (dst.loc.y as f64 - src.loc.y * sy) as f32);
        let c = self.clip;
        frame.override_default_tex_program(
            self.program.clone(),
            vec![
                Uniform::new("coord_scale", coord_scale),
                Uniform::new("coord_offset", coord_offset),
                Uniform::new("clip_rect", (c.loc.x as f32, c.loc.y as f32, c.size.w as f32, c.size.h as f32)),
                Uniform::new("radius", self.radius as f32),
            ],
        );
        let res = self.inner.draw(frame, src, dst, damage, opaque_regions, cache);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // Clipped content can't be scanned out directly.
        None
    }
}

impl crate::state::XrossState {
    /// The custom shaders, compiled on first use. `None` if they don't compile (then
    /// windows are simply drawn square and without shadows).
    pub fn shaders(&self, renderer: &mut GlesRenderer) -> Option<Shaders> {
        let mut slot = self.shaders.borrow_mut();
        if slot.is_none() {
            match Shaders::compile(renderer) {
                Ok(s) => *slot = Some(s),
                Err(err) => {
                    tracing::warn!(?err, "custom shaders failed to compile");
                    return None;
                }
            }
        }
        slot.clone()
    }

    /// Whether a window gets rounded corners and a shadow: it has our title bar and
    /// isn't fullscreen, maximized, or an edge-to-edge tile.
    pub fn window_rounded(&self, window: &smithay::desktop::Window) -> bool {
        self.rounded_with_title(window, crate::shell::has_ssd(window))
    }

    /// Like [`Self::window_rounded`], saying whether the window has our title bar (a
    /// closing window may have just dropped its decoration).
    pub fn rounded_with_title(&self, window: &smithay::desktop::Window, title: bool) -> bool {
        use crate::layout::snap::Slot;
        let config = crate::config::current();
        if config.appearance.corner_radius <= 0.0 || !title {
            return false;
        }
        if crate::shell::snap::is_fullscreen(window) {
            return false;
        }
        match self.slot_of(window) {
            None => true,
            Some(Slot::Maximized) => false,
            Some(_) => config.layout.gaps > 0,
        }
    }
}
