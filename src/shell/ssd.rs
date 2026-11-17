//! Server-side decorations: a title bar (rounded top corners, the window title, a round
//! close button) drawn directly above the window's content, plus the soft shadow
//! behind decorated windows. Everything is drawn with the shaders in `crate::shaders`.

use std::cell::RefCell;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::{GlesPixelProgram, GlesRenderer, Uniform, element::PixelShaderElement},
        },
    },
    desktop::Window,
    utils::{Logical, Point, Rectangle, Scale, Size, Transform},
};

use crate::shaders::Shaders;

pub const TITLE_HEIGHT: i32 = 26;
/// The close button's (hover) circle; the icon sits inside it.
const CLOSE_SIZE: i32 = 20;
const CLOSE_ICON: i32 = 16;
/// Behind the close icon while hovered.
const CLOSE_HOVER_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.16];
const CLOSE_MARGIN: i32 = (TITLE_HEIGHT - CLOSE_SIZE) / 2;

const BAR_FOCUSED: [f32; 4] = [0.20, 0.21, 0.25, 1.0];
const BAR_UNFOCUSED: [f32; 4] = [0.13, 0.13, 0.15, 1.0];
const CLOSE_COLOR: [f32; 4] = [0.85, 0.33, 0.33, 1.0];
const CLOSE_HOVER: [f32; 4] = [1.0, 0.42, 0.42, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationHit {
    TitleBar,
    Close,
}

/// Title bar rectangle for a window whose content occupies `content`.
pub fn title_rect(content: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    Rectangle::new((content.loc.x, content.loc.y - TITLE_HEIGHT).into(), (content.size.w, TITLE_HEIGHT).into())
}

pub fn close_rect(content: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let bar = title_rect(content);
    Rectangle::new(
        (bar.loc.x + bar.size.w - CLOSE_MARGIN - CLOSE_SIZE, bar.loc.y + CLOSE_MARGIN).into(),
        (CLOSE_SIZE, CLOSE_SIZE).into(),
    )
}

pub fn hit_test(content: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> Option<DecorationHit> {
    let bar = title_rect(content);
    if !bar.to_f64().contains(pos) {
        return None;
    }
    // Generous hit target for the close button: the full bar height at the right edge.
    let close = close_rect(content);
    let close_zone = Rectangle::<i32, Logical>::new(
        (close.loc.x - CLOSE_MARGIN, bar.loc.y).into(),
        (bar.loc.x + bar.size.w - (close.loc.x - CLOSE_MARGIN), TITLE_HEIGHT).into(),
    );
    if close_zone.to_f64().contains(pos) { Some(DecorationHit::Close) } else { Some(DecorationHit::TitleBar) }
}

/// Whether the window was last drawn with our title bar. Clients often drop their
/// decoration object right before closing, so the close snapshot can't rely on the
/// current mode.
#[derive(Default)]
struct DrawnWithSsd(std::cell::Cell<bool>);

pub fn set_drawn(window: &Window, drawn: bool) {
    window.user_data().insert_if_missing(DrawnWithSsd::default);
    window.user_data().get::<DrawnWithSsd>().unwrap().0.set(drawn);
}

pub fn drawn(window: &Window) -> bool {
    window.user_data().get::<DrawnWithSsd>().is_some_and(|d| d.0.get())
}

smithay::backend::renderer::element::render_elements! {
    pub DecorationElement<=GlesRenderer>;
    Shader=PixelShaderElement,
    Text=MemoryRenderBufferRenderElement<GlesRenderer>,
}

const TITLE_PAD: i32 = 10;
const TEXT_FOCUSED: [u8; 3] = [225, 226, 232];
const TEXT_UNFOCUSED: [u8; 3] = [140, 142, 152];
/// A shader element kept across frames so its id is stable; rebuilt only when its
/// alpha changes, re-uploaded only when its uniforms change.
#[derive(Default)]
struct CachedShader {
    element: Option<PixelShaderElement>,
    uniforms: Vec<Uniform<'static>>,
    alpha: f32,
}

impl CachedShader {
    fn get(
        &mut self,
        program: &GlesPixelProgram,
        area: Rectangle<i32, Logical>,
        uniforms: Vec<Uniform<'static>>,
        alpha: f32,
    ) -> PixelShaderElement {
        match self.element.as_mut().filter(|_| self.alpha == alpha) {
            Some(e) => {
                e.resize(area, None);
                if self.uniforms != uniforms {
                    e.update_uniforms(uniforms.clone());
                    self.uniforms = uniforms;
                }
                e.clone()
            }
            None => {
                let e =
                    PixelShaderElement::new(program.clone(), area, None, alpha, uniforms.clone(), Kind::Unspecified);
                self.element = Some(e.clone());
                self.uniforms = uniforms;
                self.alpha = alpha;
                e
            }
        }
    }
}

/// Rendered title text: (title, scale, focused), the buffer, and its logical size.
/// (title, scale, focused, "font\").
/// The size is the logical size to draw at; the rectangle covers the whole buffer.
type TitleText = ((String, i32, bool, String), MemoryRenderBuffer, Size<i32, Logical>, Rectangle<f64, Logical>);
/// (icon theme, scale, colour).
type IconKey = (String, i32, [u8; 3]);

/// Per-window decoration state, in the window's user data.
#[derive(Default)]
struct DecorationCache {
    bar: CachedShader,
    close: CachedShader,
    shadow: CachedShader,
    title: Option<TitleText>,
    /// (theme, scale, colour) -> the close icon, if the theme has one.
    close_icon: Option<(IconKey, Option<MemoryRenderBuffer>)>,
}

fn with_cache<T>(window: &Window, f: impl FnOnce(&mut DecorationCache) -> T) -> T {
    window.user_data().insert_if_missing(|| RefCell::new(DecorationCache::default()));
    f(&mut window.user_data().get::<RefCell<DecorationCache>>().unwrap().borrow_mut())
}

pub struct DecorationStyle {
    pub focused: bool,
    pub close_hovered: bool,
    /// Round the top corners (floating windows; not maximized or edge-to-edge tiles).
    pub rounded: bool,
}

fn rgba(c: [f32; 4]) -> (f32, f32, f32, f32) {
    (c[0], c[1], c[2], c[3])
}

/// Render elements for a window's title bar, front-to-back. `content` is in global
/// logical coordinates (`output_loc` is subtracted).
///
/// Pixel shaders get their element's *logical* area as `size`, so shapes (radii) are
/// given in logical units, unaffected by output scale or later rescaling.
#[allow(clippy::too_many_arguments)]
pub fn render_elements(
    renderer: &mut GlesRenderer,
    shaders: &Shaders,
    window: &Window,
    content: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    style: DecorationStyle,
    alpha: f32,
) -> Vec<DecorationElement> {
    let bar = title_rect(content);
    let close = close_rect(content);
    let local = |r: Rectangle<i32, Logical>| Rectangle::new(r.loc - output_loc, r.size);
    let radius = if style.rounded { crate::config::current().appearance.corner_radius as f32 } else { 0.0 };
    let int_scale = scale.x.ceil().max(1.0) as i32;
    let title = super::switcher::window_title(window);

    with_cache(window, |cache| {
        let mut out = Vec::new();

        // Title text, centred in the bar. The close button's space is kept clear on both
        // sides, so the text stays centred and never runs under it.
        // A closing window often clears its title first; keep showing the last real one.
        let lost_title = title == "Untitled" && cache.title.is_some();
        let appearance = crate::config::current().appearance.clone();
        let font_px = appearance.font_size.clamp(6.0, 32.0) as f32;
        let scale_key = (scale.x * 1000.0).round() as i32;
        let key = (title.clone(), scale_key, style.focused, format!("{}@{font_px}", appearance.font));
        if !lost_title && cache.title.as_ref().is_none_or(|(k, ..)| *k != key) {
            let max_w = bar.size.w - 2 * (TITLE_PAD + CLOSE_SIZE + 2 * CLOSE_MARGIN);
            let color = if style.focused { TEXT_FOCUSED } else { TEXT_UNFOCUSED };
            cache.title = (max_w > 8)
                .then(|| crate::ui::text::with_ui_font(|t| t.render(&title, font_px, max_w, scale.x, color)))
                .flatten()
                .map(|img| {
                    let buf = MemoryRenderBuffer::from_slice(
                        &img.pixels,
                        Fourcc::Argb8888,
                        (img.width, img.height),
                        1,
                        Transform::Normal,
                        None,
                    );
                    let whole = Rectangle::from_size(Size::from((img.width as f64, img.height as f64)));
                    (key, buf, Size::from((img.logical_width, img.logical_height)), whole)
                });
        }
        if let Some((_, buf, size, whole)) = &cache.title {
            let bar = local(bar);
            let loc = bar.loc.to_f64()
                + Point::from(((bar.size.w - size.w) as f64 / 2.0, (TITLE_HEIGHT - size.h) as f64 / 2.0));
            // On a whole physical pixel, at exactly the image's size: no resampling.
            let loc = loc.to_physical(scale).to_i32_round::<i32>().to_f64();
            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                loc,
                buf,
                Some(alpha),
                // The whole buffer (without this it'd show a `size`-sized corner of it).
                Some(*whole),
                Some(*size),
                Kind::Unspecified,
            ) {
                out.push(DecorationElement::Text(e));
            }
        }

        // Close button: the theme's close icon in the text colour, on a soft circle when
        // hovered. Without an icon, a red dot.
        let theme = crate::config::current().appearance.icon_theme.clone();
        let icon_color = if style.focused { TEXT_FOCUSED } else { TEXT_UNFOCUSED };
        let icon_key = (theme.clone(), int_scale, icon_color);
        if cache.close_icon.as_ref().is_none_or(|(k, _)| *k != icon_key) {
            let names = ["window-close-symbolic", "window-close"];
            let buf = crate::ui::icons::icon(&theme, &names, CLOSE_ICON * int_scale, icon_color).map(|img| {
                MemoryRenderBuffer::from_slice(
                    &img.pixels,
                    Fourcc::Argb8888,
                    (img.size, img.size),
                    int_scale,
                    Transform::Normal,
                    None,
                )
            });
            cache.close_icon = Some((icon_key, buf));
        }
        let r = CLOSE_SIZE as f32 / 2.0;
        let circle = |color: [f32; 4]| {
            vec![
                Uniform::new("color", rgba(color)),
                Uniform::new("radii", (r, r, r, r)),
                Uniform::new("extend", 0.0f32),
            ]
        };
        match cache.close_icon.as_ref().and_then(|(_, b)| b.as_ref()) {
            Some(buf) => {
                let pad = (CLOSE_SIZE - CLOSE_ICON) / 2;
                let loc = (local(close).loc + Point::from((pad, pad))).to_f64().to_physical(scale);
                if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc,
                    buf,
                    Some(alpha),
                    None,
                    Some(Size::from((CLOSE_ICON, CLOSE_ICON))),
                    Kind::Unspecified,
                ) {
                    out.push(DecorationElement::Text(e));
                }
                if style.close_hovered {
                    out.push(DecorationElement::Shader(cache.close.get(
                        &shaders.rounded_rect,
                        local(close),
                        circle(CLOSE_HOVER_BG),
                        alpha,
                    )));
                }
            }
            None => {
                let color = if style.close_hovered { CLOSE_HOVER } else { CLOSE_COLOR };
                out.push(DecorationElement::Shader(cache.close.get(
                    &shaders.rounded_rect,
                    local(close),
                    circle(color),
                    alpha,
                )));
            }
        }

        // The bar.
        let bar_color = if style.focused { BAR_FOCUSED } else { BAR_UNFOCUSED };
        out.push(DecorationElement::Shader(cache.bar.get(
            &shaders.rounded_rect,
            local(bar),
            vec![
                Uniform::new("color", rgba(bar_color)),
                Uniform::new("radii", (radius, radius, 0.0, 0.0)),
                // The window continues below the bar, so round it as part of that shape.
                Uniform::new("extend", radius * 2.0),
            ],
            alpha,
        )));
        out
    })
}

/// The soft shadow behind a decorated window. `frame` is the whole window (title bar
/// included) in output-local logical coordinates as drawn; `radius` (logical, as drawn)
/// matches its corners.
pub fn shadow_element(
    shaders: &Shaders,
    window: &Window,
    frame: Rectangle<f64, Logical>,
    radius: f64,
    alpha: f32,
) -> PixelShaderElement {
    let appearance = &crate::config::current().appearance;
    let color = crate::config::parse_color(&appearance.shadow_color).unwrap_or([0.0, 0.0, 0.0, 0.5]);
    // How far it reaches, and how far down it's offset (a light from slightly above).
    let reach = appearance.shadow_size.clamp(1.0, 200.0).round() as i32;
    let offset = reach / 6;
    let frame = frame.to_i32_round::<i32>();
    let area = Rectangle::new(
        (frame.loc.x - reach, frame.loc.y - reach + offset).into(),
        (frame.size.w + 2 * reach, frame.size.h + 2 * reach).into(),
    );
    let px = |v: i32| v as f32;
    let rect = (px(reach), px(reach - offset), px(frame.size.w), px(frame.size.h));
    with_cache(window, |cache| {
        cache.shadow.get(
            &shaders.shadow,
            area,
            vec![
                Uniform::new("shadow_color", rgba(color)),
                Uniform::new("rect", rect),
                Uniform::new("radius", radius as f32),
                Uniform::new("softness", px(reach)),
            ],
            alpha,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> Rectangle<i32, Logical> {
        Rectangle::new((100, 100).into(), (400, 300).into())
    }

    #[test]
    fn title_bar_sits_above_content() {
        let bar = title_rect(content());
        assert_eq!(bar.loc, Point::from((100, 100 - TITLE_HEIGHT)));
        assert_eq!(bar.size, Size::from((400, TITLE_HEIGHT)));
    }

    #[test]
    fn hit_testing() {
        let c = content();
        assert_eq!(hit_test(c, (150.0, 90.0).into()), Some(DecorationHit::TitleBar));
        assert_eq!(hit_test(c, (495.0, 90.0).into()), Some(DecorationHit::Close));
        assert_eq!(hit_test(c, (150.0, 150.0).into()), None, "content is not decoration");
        assert_eq!(hit_test(c, (150.0, 60.0).into()), None, "above the bar");
    }
}
