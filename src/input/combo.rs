//! Snap combo: Mod+Right then Mod+Up in quick succession snaps to the upper-right
//! quadrant (either order works).
//!
//! The first press acts immediately (half, maximize or restore), so there's no input
//! lag. A follow-up press within the timeout *rewinds* to the layout from before the
//! first press and applies the combined target instead, so the intermediate step
//! never leaves windows displaced.

use std::time::Duration;

use super::binds::Dir;
use crate::layout::snap::{Col, Row, Slot};

pub const COMBO_TIMEOUT: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComboResult {
    /// Where the window should go (`None` = floating).
    pub target: Option<Slot>,
    /// Apply `target` to the layout snapshot from before the combo started.
    pub rewind: bool,
}

#[derive(Debug)]
struct Pending<K> {
    key: K,
    col: Option<Col>,
    row: Option<Row>,
    at: Duration,
}

#[derive(Debug)]
pub struct SnapCombo<K> {
    pending: Option<Pending<K>>,
    timeout: Duration,
}

impl<K> Default for SnapCombo<K> {
    fn default() -> Self {
        Self { pending: None, timeout: COMBO_TIMEOUT }
    }
}

impl<K: PartialEq> SnapCombo<K> {
    /// A snap press for window `key` at time `now`.
    pub fn press(&mut self, key: K, dir: Dir, now: Duration) -> ComboResult {
        let pending = self.pending.take().filter(|p| p.key == key && now.saturating_sub(p.at) <= self.timeout);
        let rewind = pending.is_some();
        let (mut col, mut row) = pending.map(|p| (p.col, p.row)).unwrap_or_default();
        match dir {
            Dir::Left => col = Some(Col::Left),
            Dir::Right => col = Some(Col::Right),
            Dir::Up => row = Some(Row::Top),
            Dir::Down => row = Some(Row::Bottom),
        }
        let target = match (col, row) {
            (Some(c), Some(r)) => Some(Slot::Quarter(c, r)),
            (Some(c), None) => Some(Slot::Half(c)),
            (None, Some(Row::Top)) => Some(Slot::Maximized),
            (None, _) => None,
        };
        self.pending = Some(Pending { key, col, row, at: now });
        ComboResult { target, rewind }
    }

    pub fn cancel(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::snap::{Col::*, Row::*, Slot::*};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn single_presses() {
        let mut c = SnapCombo::default();
        assert_eq!(c.press(1, Dir::Left, ms(0)), ComboResult { target: Some(Half(Left)), rewind: false });
        assert_eq!(c.press(1, Dir::Up, ms(1000)), ComboResult { target: Some(Maximized), rewind: false });
        assert_eq!(c.press(1, Dir::Down, ms(2000)), ComboResult { target: None, rewind: false });
    }

    #[test]
    fn right_then_up_is_upper_right() {
        let mut c = SnapCombo::default();
        c.press(1, Dir::Right, ms(0));
        assert_eq!(c.press(1, Dir::Up, ms(200)), ComboResult { target: Some(Quarter(Right, Top)), rewind: true });
    }

    #[test]
    fn either_order_works() {
        let mut c = SnapCombo::default();
        c.press(1, Dir::Up, ms(0));
        assert_eq!(c.press(1, Dir::Left, ms(100)).target, Some(Quarter(Left, Top)));
        let mut c = SnapCombo::default();
        c.press(1, Dir::Down, ms(0));
        assert_eq!(c.press(1, Dir::Right, ms(100)).target, Some(Quarter(Right, Bottom)));
    }

    #[test]
    fn too_slow_is_two_separate_snaps() {
        let mut c = SnapCombo::default();
        c.press(1, Dir::Right, ms(0));
        assert_eq!(c.press(1, Dir::Up, ms(400)), ComboResult { target: Some(Maximized), rewind: false });
    }

    #[test]
    fn chains_keep_refining_while_fast() {
        let mut c = SnapCombo::default();
        c.press(1, Dir::Right, ms(0));
        c.press(1, Dir::Up, ms(200));
        // The timeout is measured from the latest press.
        assert_eq!(c.press(1, Dir::Down, ms(450)).target, Some(Quarter(Right, Bottom)));
        assert_eq!(c.press(1, Dir::Left, ms(700)).target, Some(Quarter(Left, Bottom)));
    }

    #[test]
    fn a_different_window_starts_fresh() {
        let mut c = SnapCombo::default();
        c.press(1, Dir::Right, ms(0));
        assert_eq!(c.press(2, Dir::Up, ms(100)), ComboResult { target: Some(Maximized), rewind: false });
    }

    #[test]
    fn rewind_keeps_the_displaced_window_in_its_column() {
        use crate::layout::snap::Tiling;
        let mut layout = Tiling::default();
        layout.assign(&'b', Some(Half(Right)));
        let snapshot = layout.clone();

        let mut c = SnapCombo::default();
        layout.assign(&'a', c.press('a', Dir::Right, ms(0)).target);
        assert_eq!(layout.slot_of(&'b'), None, "the first press displaces b");

        let r = c.press('a', Dir::Up, ms(100));
        assert!(r.rewind);
        let mut layout = snapshot;
        layout.assign(&'a', r.target);
        assert_eq!(layout.slot_of(&'a'), Some(Quarter(Right, Top)));
        assert_eq!(layout.slot_of(&'b'), Some(Quarter(Right, Bottom)), "b shares the column instead");
    }

    #[test]
    fn cancel_ends_the_combo() {
        let mut c = SnapCombo::default();
        c.press(1, Dir::Right, ms(0));
        c.cancel();
        assert_eq!(c.press(1, Dir::Up, ms(100)).target, Some(Maximized));
    }
}
