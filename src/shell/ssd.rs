//! Minimal server-side decorations: a thin title bar with a close button,
//! drawn directly above the window's content.

use std::cell::RefCell;

use smithay::{
    backend::renderer::element::{
        Kind,
        solid::{SolidColorBuffer, SolidColorRenderElement},
    },
    desktop::Window,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
};

pub const TITLE_HEIGHT: i32 = 26;
const CLOSE_SIZE: i32 = 14;
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

/// Per-window decoration buffers, kept in the window's user data so their ids and
/// commit counters are stable across frames (keeps damage tracking minimal).
#[derive(Default)]
struct DecorationBuffers {
    bar: SolidColorBuffer,
    close: SolidColorBuffer,
}

pub struct DecorationStyle {
    pub focused: bool,
    pub close_hovered: bool,
}

/// Render elements for a window's title bar, front-to-back.
/// `content` is in global logical coordinates; `output_loc` is subtracted.
pub fn render_elements(
    window: &Window,
    content: Rectangle<i32, Logical>,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    style: DecorationStyle,
    alpha: f32,
) -> Vec<SolidColorRenderElement> {
    window.user_data().insert_if_missing(|| RefCell::new(DecorationBuffers::default()));
    let mut bufs = window.user_data().get::<RefCell<DecorationBuffers>>().unwrap().borrow_mut();

    let bar = title_rect(content);
    let close = close_rect(content);
    bufs.bar.update(bar.size, if style.focused { BAR_FOCUSED } else { BAR_UNFOCUSED });
    bufs.close
        .update(Size::from((CLOSE_SIZE, CLOSE_SIZE)), if style.close_hovered { CLOSE_HOVER } else { CLOSE_COLOR });

    let to_phys =
        |p: Point<i32, Logical>| -> Point<i32, Physical> { (p - output_loc).to_physical_precise_round(scale) };

    vec![
        SolidColorRenderElement::from_buffer(&bufs.close, to_phys(close.loc), scale, alpha, Kind::Unspecified),
        SolidColorRenderElement::from_buffer(&bufs.bar, to_phys(bar.loc), scale, alpha, Kind::Unspecified),
    ]
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
