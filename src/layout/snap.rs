//! Snap layouts: a 2x2, column-based grid plus maximize.
//!
//! Each column (left/right) holds either one full-height window (`Half`) or a
//! top/bottom pair (`Quarter`s). `Maximized` covers the whole area and never
//! conflicts with anything. Windows not in the [`Tiling`] are floating.
//!
//! Rules when a window `k` moves from slot `P` to slot `T`:
//! - A quarter snapped onto a full-height window shrinks that window to the
//!   other quarter of its column (this is how 3-window layouts are built).
//! - Otherwise windows already occupying `T` are displaced. If `P` was a tile
//!   that doesn't overlap `T`, they swap into `P`; if not, they float.
//! - When `k` leaves a quarter and nobody takes its place, the other quarter of
//!   that column expands to the full height.

use smithay::utils::{Logical, Rectangle};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Col {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Row {
    Top,
    Bottom,
}

impl Row {
    pub fn other(self) -> Row {
        match self {
            Row::Top => Row::Bottom,
            Row::Bottom => Row::Top,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Maximized,
    Half(Col),
    Quarter(Col, Row),
}

impl Slot {
    fn col(self) -> Option<Col> {
        match self {
            Slot::Maximized => None,
            Slot::Half(c) | Slot::Quarter(c, _) => Some(c),
        }
    }

    /// Whether two grid slots cover any of the same area. `Maximized` floats above
    /// the grid, so it overlaps nothing.
    fn overlaps(self, other: Slot) -> bool {
        match (self, other) {
            (Slot::Maximized, _) | (_, Slot::Maximized) => false,
            (Slot::Quarter(c1, r1), Slot::Quarter(c2, r2)) => c1 == c2 && r1 == r2,
            (a, b) => a.col() == b.col(),
        }
    }

    /// Like [`Slot::rect`], with `gap` px around the area and between tiles. Maximized
    /// windows ignore gaps and fill the area.
    pub fn rect_with_gaps(self, area: Rectangle<i32, Logical>, gap: i32) -> Rectangle<i32, Logical> {
        let gap = gap.max(0);
        if self == Slot::Maximized || gap == 0 {
            return self.rect(area);
        }
        let inner = Rectangle::new(
            (area.loc.x + gap, area.loc.y + gap).into(),
            ((area.size.w - 2 * gap).max(1), (area.size.h - 2 * gap).max(1)).into(),
        );
        let r = self.rect(inner);
        // Edges shared with another tile get half the gap each, so tiles end up `gap` apart.
        let half = gap / 2;
        let (left, top) = (r.loc.x > inner.loc.x, r.loc.y > inner.loc.y);
        let right = r.loc.x + r.size.w < inner.loc.x + inner.size.w;
        let bottom = r.loc.y + r.size.h < inner.loc.y + inner.size.h;
        let (x0, y0) = (r.loc.x + if left { gap - half } else { 0 }, r.loc.y + if top { gap - half } else { 0 });
        let x1 = r.loc.x + r.size.w - if right { half } else { 0 };
        let y1 = r.loc.y + r.size.h - if bottom { half } else { 0 };
        Rectangle::new((x0, y0).into(), ((x1 - x0).max(1), (y1 - y0).max(1)).into())
    }

    /// The frame rectangle (title bar included) of this slot within `area`.
    pub fn rect(self, area: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        let (x, y, w, h) = (area.loc.x, area.loc.y, area.size.w, area.size.h);
        let (lw, th) = (w / 2, h / 2);
        let col = |c: Col| match c {
            Col::Left => (x, lw),
            Col::Right => (x + lw, w - lw),
        };
        let row = |r: Row| match r {
            Row::Top => (y, th),
            Row::Bottom => (y + th, h - th),
        };
        match self {
            Slot::Maximized => area,
            Slot::Half(c) => {
                let (cx, cw) = col(c);
                Rectangle::new((cx, y).into(), (cw, h).into())
            }
            Slot::Quarter(c, r) => {
                let ((cx, cw), (ry, rh)) = (col(c), row(r));
                Rectangle::new((cx, ry).into(), (cw, rh).into())
            }
        }
    }
}

/// Which grid slot each tiled window occupies. `K` identifies a window.
#[derive(Debug, Clone)]
pub struct Tiling<K> {
    tiles: Vec<(K, Slot)>,
}

impl<K> Default for Tiling<K> {
    fn default() -> Self {
        Self { tiles: Vec::new() }
    }
}

impl<K: Clone + PartialEq> Tiling<K> {
    pub fn slot_of(&self, k: &K) -> Option<Slot> {
        self.tiles.iter().find(|(w, _)| w == k).map(|(_, s)| *s)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, Slot)> {
        self.tiles.iter().map(|(k, s)| (k, *s))
    }

    fn set(&mut self, k: &K, slot: Slot) {
        self.unset(k);
        self.tiles.push((k.clone(), slot));
    }

    fn unset(&mut self, k: &K) -> Option<Slot> {
        let idx = self.tiles.iter().position(|(w, _)| w == k)?;
        Some(self.tiles.remove(idx).1)
    }

    fn at(&self, slot: Slot) -> Option<K> {
        self.tiles.iter().find(|(_, s)| *s == slot).map(|(k, _)| k.clone())
    }

    /// Take `k` out of the grid (it floats, closes, or leaves the workspace).
    pub fn remove(&mut self, k: &K) -> Option<Slot> {
        let prev = self.unset(k)?;
        self.expand_sibling(prev);
        Some(prev)
    }

    /// If `left` was a quarter that is now empty, grow the other quarter of its column.
    fn expand_sibling(&mut self, left: Slot) {
        if let Slot::Quarter(c, r) = left
            && self.at(left).is_none()
            && let Some(sibling) = self.at(Slot::Quarter(c, r.other()))
        {
            self.set(&sibling, Slot::Half(c));
        }
    }

    /// Move `k` to `target` (`None` floats it), rearranging the others per the module rules.
    pub fn assign(&mut self, k: &K, target: Option<Slot>) {
        let prev = self.slot_of(k);
        if prev == target {
            return;
        }
        let Some(target) = target else {
            self.remove(k);
            return;
        };
        self.unset(k);

        let mut displaced: Vec<(K, Slot)> = Vec::new();
        match target {
            Slot::Maximized => {}
            Slot::Half(c) => {
                displaced = self.tiles.iter().filter(|(_, s)| s.col() == Some(c)).cloned().collect();
            }
            Slot::Quarter(c, r) => {
                if let Some(full) = self.at(Slot::Half(c)) {
                    self.set(&full, Slot::Quarter(c, r.other()));
                } else if let Some(o) = self.at(target) {
                    displaced.push((o, target));
                }
            }
        }
        for (o, _) in &displaced {
            self.unset(o);
        }
        self.set(k, target);

        // Swap the displaced windows into where `k` came from, if that space is free.
        if let Some(p) = prev.filter(|p| *p != Slot::Maximized && !p.overlaps(target)) {
            match (p, displaced.as_slice()) {
                (_, [(o, _)]) => self.set(o, p),
                (Slot::Half(pc), pair) => {
                    for (o, s) in pair {
                        if let Slot::Quarter(_, r) = s {
                            self.set(o, Slot::Quarter(pc, *r));
                        }
                    }
                }
                (Slot::Quarter(_, pr), pair) => {
                    if let Some((o, _)) = pair.iter().find(|(_, s)| matches!(s, Slot::Quarter(_, r) if *r == pr)) {
                        self.set(o, p);
                    }
                }
                (Slot::Maximized, _) => {}
            }
            self.expand_sibling(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Col::*, Row::*, Slot::*, *};

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new((0, 30).into(), (1001, 701).into())
    }

    fn tiling(tiles: &[(char, Slot)]) -> Tiling<char> {
        let mut t = Tiling::default();
        for (k, s) in tiles {
            t.set(k, *s);
        }
        t
    }

    fn sorted(t: &Tiling<char>) -> Vec<(char, Slot)> {
        let mut v: Vec<_> = t.iter().map(|(k, s)| (*k, s)).collect();
        v.sort_by_key(|(k, _)| *k);
        v
    }

    #[test]
    fn slot_rects_tile_the_area_exactly() {
        let a = area();
        assert_eq!(Maximized.rect(a), a);
        let (l, r) = (Half(Left).rect(a), Half(Right).rect(a));
        assert_eq!(l, Rectangle::new((0, 30).into(), (500, 701).into()));
        assert_eq!(r, Rectangle::new((500, 30).into(), (501, 701).into()));
        let (ur, lr) = (Quarter(Right, Top).rect(a), Quarter(Right, Bottom).rect(a));
        assert_eq!(ur, Rectangle::new((500, 30).into(), (501, 350).into()));
        assert_eq!(lr, Rectangle::new((500, 380).into(), (501, 351).into()));
    }

    #[test]
    fn gaps_go_around_and_between_tiles() {
        let a = Rectangle::new((0, 0).into(), (1000, 600).into());
        let g = 10;
        let l = Half(Left).rect_with_gaps(a, g);
        let r = Half(Right).rect_with_gaps(a, g);
        assert_eq!(l.loc, (10, 10).into(), "outer gap");
        assert_eq!(l.size.h, 580);
        assert_eq!(r.loc.x - (l.loc.x + l.size.w), g, "inner gap between the halves");
        assert_eq!(r.loc.x + r.size.w, 990, "outer gap on the right");
        let ur = Quarter(Right, Top).rect_with_gaps(a, g);
        let lr = Quarter(Right, Bottom).rect_with_gaps(a, g);
        assert_eq!(lr.loc.y - (ur.loc.y + ur.size.h), g, "inner gap between the quarters");
        assert_eq!(Maximized.rect_with_gaps(a, g), a, "maximized fills the area");
        assert_eq!(Half(Left).rect_with_gaps(a, 0), Half(Left).rect(a));
    }

    #[test]
    fn halves_side_by_side() {
        let mut t = tiling(&[]);
        t.assign(&'a', Some(Half(Left)));
        t.assign(&'b', Some(Half(Right)));
        assert_eq!(sorted(&t), vec![('a', Half(Left)), ('b', Half(Right))]);
    }

    #[test]
    fn quarter_onto_full_column_shrinks_it() {
        let mut t = tiling(&[('a', Half(Left)), ('b', Half(Right))]);
        t.assign(&'c', Some(Quarter(Right, Top)));
        assert_eq!(sorted(&t), vec![('a', Half(Left)), ('b', Quarter(Right, Bottom)), ('c', Quarter(Right, Top))]);
    }

    #[test]
    fn four_quadrants() {
        let mut t = tiling(&[]);
        t.assign(&'a', Some(Half(Left)));
        t.assign(&'b', Some(Half(Right)));
        t.assign(&'c', Some(Quarter(Left, Bottom)));
        t.assign(&'d', Some(Quarter(Right, Bottom)));
        assert_eq!(
            sorted(&t),
            vec![
                ('a', Quarter(Left, Top)),
                ('b', Quarter(Right, Top)),
                ('c', Quarter(Left, Bottom)),
                ('d', Quarter(Right, Bottom)),
            ]
        );
    }

    #[test]
    fn leaving_a_quarter_expands_its_sibling() {
        let mut t = tiling(&[('a', Half(Left)), ('b', Quarter(Right, Top)), ('c', Quarter(Right, Bottom))]);
        t.remove(&'b');
        assert_eq!(sorted(&t), vec![('a', Half(Left)), ('c', Half(Right))]);
    }

    #[test]
    fn a_lone_quarter_stays_a_quarter() {
        let mut t = tiling(&[]);
        t.assign(&'a', Some(Quarter(Right, Top)));
        assert_eq!(sorted(&t), vec![('a', Quarter(Right, Top))]);
    }

    #[test]
    fn half_onto_half_swaps() {
        let mut t = tiling(&[('a', Half(Left)), ('b', Half(Right))]);
        t.assign(&'a', Some(Half(Right)));
        assert_eq!(sorted(&t), vec![('a', Half(Right)), ('b', Half(Left))]);
    }

    #[test]
    fn half_onto_pair_from_a_half_swaps_the_pair() {
        let mut t = tiling(&[('a', Half(Left)), ('b', Quarter(Right, Top)), ('c', Quarter(Right, Bottom))]);
        t.assign(&'a', Some(Half(Right)));
        assert_eq!(sorted(&t), vec![('a', Half(Right)), ('b', Quarter(Left, Top)), ('c', Quarter(Left, Bottom))]);
    }

    #[test]
    fn quarter_swap_within_a_column() {
        let mut t = tiling(&[('a', Quarter(Left, Top)), ('b', Quarter(Left, Bottom))]);
        t.assign(&'a', Some(Quarter(Left, Bottom)));
        assert_eq!(sorted(&t), vec![('a', Quarter(Left, Bottom)), ('b', Quarter(Left, Top))]);
    }

    #[test]
    fn floating_window_displaces_occupant_to_floating() {
        let mut t = tiling(&[('a', Half(Left))]);
        t.assign(&'b', Some(Half(Left)));
        assert_eq!(sorted(&t), vec![('b', Half(Left))]);
    }

    #[test]
    fn quarter_growing_to_its_half_floats_the_sibling() {
        let mut t = tiling(&[('a', Quarter(Right, Top)), ('b', Quarter(Right, Bottom))]);
        t.assign(&'a', Some(Half(Right)));
        assert_eq!(sorted(&t), vec![('a', Half(Right))]);
    }

    #[test]
    fn quarter_to_other_half_expands_old_sibling() {
        let mut t = tiling(&[('a', Quarter(Right, Top)), ('b', Quarter(Right, Bottom))]);
        t.assign(&'a', Some(Half(Left)));
        assert_eq!(sorted(&t), vec![('a', Half(Left)), ('b', Half(Right))]);
    }

    #[test]
    fn maximize_leaves_the_grid_and_others_fill_in() {
        let mut t = tiling(&[('a', Quarter(Right, Top)), ('b', Quarter(Right, Bottom)), ('c', Half(Left))]);
        t.assign(&'a', Some(Maximized));
        assert_eq!(sorted(&t), vec![('a', Maximized), ('b', Half(Right)), ('c', Half(Left))]);
        // Maximized windows don't conflict with each other or the grid.
        t.assign(&'d', Some(Maximized));
        assert_eq!(t.slot_of(&'d'), Some(Maximized));
        assert_eq!(t.slot_of(&'a'), Some(Maximized));
    }

    #[test]
    fn float_removes() {
        let mut t = tiling(&[('a', Half(Left))]);
        t.assign(&'a', None);
        assert_eq!(sorted(&t), vec![]);
    }
}
