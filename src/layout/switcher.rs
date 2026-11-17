//! Alt-tab (Mod+Tab), GNOME / AATWS style.
//!
//! - Mod+Tab starts a session over the current workspace's windows in most-recently-
//!   used order, with the *previous* window selected. Releasing Mod quickly switches to
//!   it without anything being drawn (the quick toggle).
//! - Holding Mod past [`SHOW_DELAY`] shows the tiles; Tab / Shift+Tab move the
//!   selection (wrapping) and show them immediately. Releasing Mod commits; Esc cancels.

use std::time::Duration;

use smithay::utils::{Logical, Point, Rectangle, Size};

pub const SHOW_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub struct Session<K> {
    items: Vec<K>,
    index: usize,
    started: Duration,
    /// Cycling shows the tiles right away, without waiting for the delay.
    forced_visible: bool,
}

impl<K: Clone> Session<K> {
    /// `items` is in MRU order (the focused window first). `reverse` starts from the
    /// least recent (Mod+Shift+Tab). `None` if there is nothing to switch to.
    pub fn start(items: Vec<K>, reverse: bool, now: Duration) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let index = match (reverse, items.len()) {
            (_, 1) => 0,
            (false, _) => 1,
            (true, n) => n - 1,
        };
        Some(Self { items, index, started: now, forced_visible: false })
    }

    pub fn next(&mut self) {
        self.index = (self.index + 1) % self.items.len();
        self.forced_visible = true;
    }

    pub fn prev(&mut self) {
        self.index = (self.index + self.items.len() - 1) % self.items.len();
        self.forced_visible = true;
    }

    pub fn select(&mut self, index: usize) {
        if index < self.items.len() {
            self.index = index;
        }
    }

    pub fn items(&self) -> &[K] {
        &self.items
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn selected(&self) -> &K {
        &self.items[self.index]
    }

    pub fn visible(&self, now: Duration) -> bool {
        self.forced_visible || now.saturating_sub(self.started) >= SHOW_DELAY
    }

    /// Drop an item (its window closed mid-session). Returns false if none are left.
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) -> bool
    where
        K: PartialEq,
    {
        let selected = self.items[self.index].clone();
        self.items.retain(|k| keep(k));
        if self.items.is_empty() {
            return false;
        }
        // Stay on the same window if it survived, else on the same position.
        self.index = self.items.iter().position(|k| *k == selected).unwrap_or(self.index.min(self.items.len() - 1));
        true
    }
}

// --- Tile layout -------------------------------------------------------------

/// Largest thumbnail box (logical px).
pub const THUMB_MAX: (i32, i32) = (240, 160);
pub const TILE_PAD: i32 = 10;
pub const TITLE_H: i32 = 22;
pub const TILE_GAP: i32 = 8;
pub const PANEL_PAD: i32 = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    /// The whole tile (highlighted when selected).
    pub rect: Rectangle<i32, Logical>,
    /// Where the scaled window goes, aspect ratio preserved.
    pub thumb: Rectangle<i32, Logical>,
    /// Title text area under the thumbnail.
    pub title: Rectangle<i32, Logical>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub panel: Rectangle<i32, Logical>,
    pub tiles: Vec<Tile>,
}

/// Fit `size` into `max`, keeping its aspect ratio.
fn fit(size: Size<i32, Logical>, max: (i32, i32)) -> Size<i32, Logical> {
    let (w, h) = (size.w.max(1) as f64, size.h.max(1) as f64);
    let k = (max.0 as f64 / w).min(max.1 as f64 / h).min(1.0);
    Size::from((((w * k).round() as i32).max(1), ((h * k).round() as i32).max(1)))
}

/// Tiles in rows centered in `output` (output-local coordinates), wrapping when a row
/// would exceed 90% of the output width.
pub fn layout(windows: &[Size<i32, Logical>], output: Size<i32, Logical>) -> Layout {
    let tile_w = THUMB_MAX.0 + 2 * TILE_PAD;
    let tile_h = THUMB_MAX.1 + 2 * TILE_PAD + TITLE_H;
    let max_row_w = (output.w as f64 * 0.9) as i32 - 2 * PANEL_PAD;
    let per_row = (((max_row_w + TILE_GAP) / (tile_w + TILE_GAP)).max(1) as usize).min(windows.len().max(1));
    let rows = windows.len().div_ceil(per_row).max(1);

    let panel_size = Size::from((
        per_row as i32 * tile_w + (per_row as i32 - 1) * TILE_GAP + 2 * PANEL_PAD,
        rows as i32 * tile_h + (rows as i32 - 1) * TILE_GAP + 2 * PANEL_PAD,
    ));
    let panel_loc = Point::from(((output.w - panel_size.w) / 2, (output.h - panel_size.h) / 2));

    let tiles = windows
        .iter()
        .enumerate()
        .map(|(i, &size)| {
            let (row, col) = (i / per_row, i % per_row);
            // Center a short last row.
            let in_row = if row == rows - 1 { windows.len() - row * per_row } else { per_row };
            let row_w = in_row as i32 * tile_w + (in_row as i32 - 1) * TILE_GAP;
            let row_x = panel_loc.x + (panel_size.w - row_w) / 2;
            let loc = Point::from((
                row_x + col as i32 * (tile_w + TILE_GAP),
                panel_loc.y + PANEL_PAD + row as i32 * (tile_h + TILE_GAP),
            ));
            let rect = Rectangle::new(loc, (tile_w, tile_h).into());
            let t = fit(size, THUMB_MAX);
            let thumb = Rectangle::new(
                (loc.x + TILE_PAD + (THUMB_MAX.0 - t.w) / 2, loc.y + TILE_PAD + (THUMB_MAX.1 - t.h) / 2).into(),
                t,
            );
            let title = Rectangle::new(
                (loc.x + TILE_PAD, loc.y + TILE_PAD + THUMB_MAX.1 + 4).into(),
                (THUMB_MAX.0, TITLE_H - 4).into(),
            );
            Tile { rect, thumb, title }
        })
        .collect();
    Layout { panel: Rectangle::new(panel_loc, panel_size), tiles }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn quick_toggle_selects_the_previous_window() {
        let s = Session::start(vec!['a', 'b', 'c'], false, ms(0)).unwrap();
        assert_eq!(*s.selected(), 'b');
        assert!(!s.visible(ms(100)), "a quick tap draws nothing");
    }

    #[test]
    fn holding_shows_the_tiles() {
        let s = Session::start(vec!['a', 'b'], false, ms(0)).unwrap();
        assert!(s.visible(SHOW_DELAY));
    }

    #[test]
    fn tab_and_shift_tab_wrap() {
        let mut s = Session::start(vec!['a', 'b', 'c'], false, ms(0)).unwrap();
        s.next();
        assert_eq!(*s.selected(), 'c');
        assert!(s.visible(ms(1)), "cycling shows the tiles immediately");
        s.next();
        assert_eq!(*s.selected(), 'a');
        s.prev();
        s.prev();
        assert_eq!(*s.selected(), 'b');
    }

    #[test]
    fn shift_tab_first_starts_at_the_least_recent() {
        let s = Session::start(vec!['a', 'b', 'c'], true, ms(0)).unwrap();
        assert_eq!(*s.selected(), 'c');
    }

    #[test]
    fn single_and_empty() {
        assert_eq!(*Session::start(vec!['a'], false, ms(0)).unwrap().selected(), 'a');
        assert!(Session::<char>::start(vec![], false, ms(0)).is_none());
    }

    #[test]
    fn closing_windows_mid_session() {
        let mut s = Session::start(vec!['a', 'b', 'c'], false, ms(0)).unwrap();
        // 'b' selected; closing 'a' keeps the selection on 'b'.
        assert!(s.retain(|k| *k != 'a'));
        assert_eq!(*s.selected(), 'b');
        // Closing the selected one keeps the position.
        s.next(); // 'c'
        assert!(s.retain(|k| *k != 'c'));
        assert_eq!(*s.selected(), 'b');
        assert!(!s.retain(|_| false));
    }

    #[test]
    fn layout_centers_one_row() {
        let out = Size::from((1920, 1080));
        let l = layout(&[Size::from((800, 600)), Size::from((1920, 1080))], out);
        assert_eq!(l.tiles.len(), 2);
        assert_eq!(l.panel.loc.x, (out.w - l.panel.size.w) / 2);
        assert_eq!(l.tiles[0].rect.loc.y, l.tiles[1].rect.loc.y, "same row");
        for t in &l.tiles {
            assert!(t.thumb.size.w <= THUMB_MAX.0 && t.thumb.size.h <= THUMB_MAX.1);
            assert!(l.panel.contains_rect(t.rect));
        }
        // Aspect ratio kept: 800x600 -> 213x160.
        assert_eq!(l.tiles[0].thumb.size, Size::from((213, 160)));
    }

    #[test]
    fn layout_wraps_into_rows() {
        let out = Size::from((1280, 800));
        let l = layout(&[Size::from((800, 600)); 7], out);
        let rows: std::collections::BTreeSet<i32> = l.tiles.iter().map(|t| t.rect.loc.y).collect();
        assert!(rows.len() >= 2);
        assert!(l.panel.size.w <= out.w);
        for t in &l.tiles {
            assert!(l.panel.contains_rect(t.rect));
        }
    }
}
