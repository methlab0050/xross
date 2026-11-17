//! Frosted-glass blur for the overview: the backdrop (wallpaper and background colour)
//! is rendered offscreen, then blurred with the dual Kawase method: a few passes down
//! to 1/16 resolution and back up to 1/2, each sampling a handful of neighbours. That's
//! a wide, smooth blur for very little GPU time. Empty workspaces in the overview show
//! their part of the result.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Frame, Offscreen, Renderer,
            damage::OutputDamageTracker,
            element::RenderElement,
            gles::{GlesError, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
        },
    },
    utils::{Physical, Rectangle, Size, Transform},
};

/// Downsampling passes (each halves the resolution).
const DOWN_PASSES: usize = 4;
/// Upsampling passes after that; the result is at `1 / 2^(DOWN - UP)` resolution.
const UP_PASSES: usize = 3;

const DOWN: &str = r#"
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
uniform vec2 halfpixel;

void main() {
    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords - halfpixel);
    sum += texture2D(tex, v_coords + halfpixel);
    sum += texture2D(tex, v_coords + vec2(halfpixel.x, -halfpixel.y));
    sum += texture2D(tex, v_coords - vec2(halfpixel.x, -halfpixel.y));
    gl_FragColor = sum / 8.0 * alpha;
}
"#;

const UP: &str = r#"
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
uniform vec2 halfpixel;

void main() {
    vec2 h = halfpixel;
    vec4 sum = texture2D(tex, v_coords + vec2(-h.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(-h.x, h.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, h.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(h.x, h.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(h.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(h.x, -h.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, -h.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(-h.x, -h.y)) * 2.0;
    gl_FragColor = sum / 12.0 * alpha;
}
"#;

/// Compiled programs and the textures they render into, reused across frames.
pub struct Blur {
    down: GlesTexProgram,
    up: GlesTexProgram,
    /// Output size the textures were made for.
    size: Size<i32, Physical>,
    /// Full-resolution backdrop, then one texture per blur level.
    backdrop: Option<GlesTexture>,
    levels: Vec<GlesTexture>,
}

impl Blur {
    pub fn new(renderer: &mut GlesRenderer) -> Result<Self, GlesError> {
        let halfpixel = [UniformName { name: "halfpixel".into(), type_: UniformType::_2f }];
        Ok(Self {
            down: renderer.compile_custom_texture_shader(DOWN, &halfpixel)?,
            up: renderer.compile_custom_texture_shader(UP, &halfpixel)?,
            size: Size::default(),
            backdrop: None,
            levels: Vec::new(),
        })
    }

    /// How much smaller than the output the blurred texture is.
    pub fn factor() -> f64 {
        (1 << (DOWN_PASSES - UP_PASSES)) as f64
    }

    fn level_size(size: Size<i32, Physical>, level: usize) -> Size<i32, Physical> {
        Size::from(((size.w >> level).max(1), (size.h >> level).max(1)))
    }

    fn ensure_textures(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<(), GlesError> {
        if self.size == size && self.backdrop.is_some() {
            return Ok(());
        }
        self.size = size;
        self.backdrop = Some(renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())?);
        self.levels = (1..=DOWN_PASSES)
            .map(|l| {
                let s = Self::level_size(size, l);
                renderer.create_buffer(Fourcc::Abgr8888, (s.w, s.h).into())
            })
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    /// Render `elements` (the backdrop) at `size` and blur it. Returns the blurred
    /// texture, `factor()` times smaller than the output.
    pub fn render<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[E],
        size: Size<i32, Physical>,
        scale: f64,
        clear: [f32; 4],
    ) -> Result<GlesTexture, String> {
        self.ensure_textures(renderer, size).map_err(|e| e.to_string())?;
        let mut backdrop = self.backdrop.clone().ok_or("no backdrop texture")?;
        {
            let mut target = renderer.bind(&mut backdrop).map_err(|e| e.to_string())?;
            let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
            tracker.render_output(renderer, &mut target, 0, elements, clear).map_err(|e| format!("{e:?}"))?;
        }

        // Down: backdrop -> level 1 -> ... -> level N.
        let mut src = backdrop;
        for level in 1..=DOWN_PASSES {
            let dst = self.levels[level - 1].clone();
            pass(renderer, &src, dst.clone(), Self::level_size(size, level), &self.down)?;
            src = dst;
        }
        // Up: level N -> level N-1 -> ... (back up to level DOWN - UP).
        for level in (DOWN_PASSES - UP_PASSES..DOWN_PASSES).rev() {
            let dst = self.levels[level - 1].clone();
            pass(renderer, &src, dst.clone(), Self::level_size(size, level), &self.up)?;
            src = dst;
        }
        Ok(src)
    }
}

/// One blur pass: draw all of `src` into all of `dst` with `program`.
fn pass(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    mut dst: GlesTexture,
    dst_size: Size<i32, Physical>,
    program: &GlesTexProgram,
) -> Result<(), String> {
    use smithay::backend::renderer::Texture;
    let src_size = src.size();
    let halfpixel = (0.5 / src_size.w as f32, 0.5 / src_size.h as f32);
    let mut target = renderer.bind(&mut dst).map_err(|e| e.to_string())?;
    let mut frame = renderer.render(&mut target, dst_size, Transform::Normal).map_err(|e| e.to_string())?;
    let full = Rectangle::from_size(dst_size);
    frame
        .render_texture_from_to(
            src,
            Rectangle::from_size(src_size.to_f64()),
            full,
            &[full],
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &[Uniform::new("halfpixel", halfpixel)],
        )
        .map_err(|e| e.to_string())?;
    // Later passes read this texture in the same GL context, which orders them; no need
    // to wait on the sync point.
    let _sync = frame.finish().map_err(|e| e.to_string())?;
    Ok(())
}
