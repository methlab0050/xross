//! The 2D workspace grid: navigation rules and compaction.
//!
//! - A workspace is *active* when it has at least one window.
//! - From an active workspace you can move to any of its 4 neighbours; an empty
//!   workspace is created there on demand.
//! - From an inactive workspace you can only move to an *active* neighbour.
//! - After a switch, [`Grid::compact`] drops empty workspaces other than the current
//!   one and closes up rows/columns that no longer contain any active workspace
//!   (the current workspace's row and column always stay). Coordinates on the far
//!   side of a removed line, as seen from the origin, shift one step towards it:
//!   if row 1 empties out, `(0,2)` becomes `(0,1)`, `(0,3)` becomes `(0,2)`, ...
//!
//! `y` grows downwards, like screen coordinates.

use std::collections::{BTreeSet, HashMap};

use crate::input::binds::Dir;

pub type Coord = (i32, i32);

pub fn neighbour((x, y): Coord, dir: Dir) -> Coord {
    match dir {
        Dir::Left => (x - 1, y),
        Dir::Right => (x + 1, y),
        Dir::Up => (x, y - 1),
        Dir::Down => (x, y + 1),
    }
}

#[derive(Debug)]
pub struct Grid<W> {
    cells: HashMap<Coord, W>,
    current: Coord,
}

impl<W> Grid<W> {
    /// A grid holding just `first`, at the origin.
    pub fn new(first: W) -> Self {
        Self { cells: HashMap::from([((0, 0), first)]), current: (0, 0) }
    }

    pub fn current(&self) -> Coord {
        self.current
    }

    pub fn current_ws(&self) -> &W {
        &self.cells[&self.current]
    }

    pub fn get(&self, c: Coord) -> Option<&W> {
        self.cells.get(&c)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Coord, &W)> {
        self.cells.iter().map(|(c, w)| (*c, w))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Coord, &mut W)> {
        self.cells.iter_mut().map(|(c, w)| (*c, w))
    }

    /// Where a move in `dir` from the current workspace lands, if it is allowed.
    pub fn target(&self, dir: Dir, active: impl Fn(&W) -> bool) -> Option<Coord> {
        let to = neighbour(self.current, dir);
        let from_active = active(self.current_ws());
        let to_active = self.cells.get(&to).is_some_and(&active);
        (from_active || to_active).then_some(to)
    }

    /// Make `to` current, creating an empty workspace there if needed.
    pub fn switch_to(&mut self, to: Coord, make: impl FnOnce() -> W) {
        self.cells.entry(to).or_insert_with(make);
        self.current = to;
    }

    /// Drop inactive workspaces (except the current one) and close up empty
    /// rows and columns. Returns the dropped workspaces.
    pub fn compact(&mut self, active: impl Fn(&W) -> bool) -> Vec<W> {
        let current = self.current;
        let dead: Vec<Coord> =
            self.cells.iter().filter(|(c, w)| **c != current && !active(w)).map(|(c, _)| *c).collect();
        let dropped = dead.iter().filter_map(|c| self.cells.remove(c)).collect();

        let xs: BTreeSet<i32> = self.cells.keys().map(|c| c.0).collect();
        let ys: BTreeSet<i32> = self.cells.keys().map(|c| c.1).collect();
        let (gx, gy) = (gaps(&xs), gaps(&ys));
        if !gx.is_empty() || !gy.is_empty() {
            let remap = |(x, y): Coord| (close_gaps(x, &gx), close_gaps(y, &gy));
            self.cells = std::mem::take(&mut self.cells).into_iter().map(|(c, w)| (remap(c), w)).collect();
            self.current = remap(self.current);
        }
        dropped
    }
}

/// Coordinates strictly between the smallest and largest used ones that are unused.
fn gaps(used: &BTreeSet<i32>) -> Vec<i32> {
    match (used.first(), used.last()) {
        (Some(&lo), Some(&hi)) => (lo..hi).filter(|v| !used.contains(v)).collect(),
        _ => Vec::new(),
    }
}

/// Shift `v` towards the origin by the number of removed lines between it and the origin.
fn close_gaps(v: i32, removed: &[i32]) -> i32 {
    if v >= 0 {
        v - removed.iter().filter(|&&r| (0..v).contains(&r)).count() as i32
    } else {
        v + removed.iter().filter(|&&r| r > v && r < 0).count() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Workspaces in tests are just their window count, tagged with a name.
    #[derive(Debug, Clone, PartialEq)]
    struct Ws(&'static str, usize);

    fn active(w: &Ws) -> bool {
        w.1 > 0
    }

    fn grid(cells: &[(Coord, Ws)], current: Coord) -> Grid<Ws> {
        Grid { cells: cells.iter().cloned().collect(), current }
    }

    fn named(g: &Grid<Ws>) -> Vec<(Coord, &'static str)> {
        let mut v: Vec<_> = g.iter().map(|(c, w)| (c, w.0)).collect();
        v.sort();
        v
    }

    #[test]
    fn from_active_all_four_neighbours() {
        let g = grid(&[((0, 0), Ws("a", 1))], (0, 0));
        assert_eq!(g.target(Dir::Right, active), Some((1, 0)));
        assert_eq!(g.target(Dir::Left, active), Some((-1, 0)));
        assert_eq!(g.target(Dir::Up, active), Some((0, -1)));
        assert_eq!(g.target(Dir::Down, active), Some((0, 1)));
    }

    #[test]
    fn from_inactive_only_to_active_neighbours() {
        let g = grid(&[((0, 0), Ws("a", 1)), ((1, 0), Ws("empty", 0)), ((1, 1), Ws("b", 2))], (1, 0));
        assert_eq!(g.target(Dir::Left, active), Some((0, 0)));
        assert_eq!(g.target(Dir::Down, active), Some((1, 1)));
        assert_eq!(g.target(Dir::Right, active), None);
        assert_eq!(g.target(Dir::Up, active), None);
    }

    #[test]
    fn switching_creates_the_target() {
        let mut g = grid(&[((0, 0), Ws("a", 1))], (0, 0));
        g.switch_to((1, 0), || Ws("new", 0));
        assert_eq!(g.current(), (1, 0));
        assert_eq!(g.current_ws(), &Ws("new", 0));
    }

    /// The scenario from the spec: (0,0)..(0,3) active, (0,1) empties out, the user
    /// swipes away to (0,2); row 1 is removed and everything below moves up.
    #[test]
    fn collapse_row_after_last_window_closes() {
        let mut g =
            grid(&[((0, 0), Ws("a", 1)), ((0, 1), Ws("b", 0)), ((0, 2), Ws("c", 1)), ((0, 3), Ws("d", 1))], (0, 1));
        // b is inactive now, so only active neighbours are reachable.
        let to = g.target(Dir::Down, active).unwrap();
        g.switch_to(to, || unreachable!());
        let dropped = g.compact(active);
        assert_eq!(dropped, vec![Ws("b", 0)]);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((0, 1), "c"), ((0, 2), "d")]);
        assert_eq!(g.current(), (0, 1), "still on c");
    }

    #[test]
    fn row_with_another_active_workspace_stays() {
        let mut g =
            grid(&[((0, 0), Ws("a", 1)), ((0, 1), Ws("b", 0)), ((1, 1), Ws("e", 1)), ((0, 2), Ws("c", 1))], (0, 2));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((0, 2), "c"), ((1, 1), "e")]);
    }

    #[test]
    fn current_empty_workspace_and_its_lines_stay() {
        let mut g = grid(&[((0, 0), Ws("a", 1)), ((1, 0), Ws("here", 0))], (1, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((1, 0), "here")]);
    }

    #[test]
    fn leaving_an_empty_workspace_removes_it() {
        let mut g = grid(&[((0, 0), Ws("a", 1))], (0, 0));
        g.switch_to((1, 0), || Ws("tmp", 0));
        g.switch_to((0, 0), || unreachable!());
        assert_eq!(g.compact(active), vec![Ws("tmp", 0)]);
        assert_eq!(named(&g), vec![((0, 0), "a")]);
    }

    #[test]
    fn columns_collapse_too() {
        let mut g = grid(&[((0, 0), Ws("a", 1)), ((2, 0), Ws("c", 1)), ((3, 0), Ws("d", 1))], (3, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((1, 0), "c"), ((2, 0), "d")]);
        assert_eq!(g.current(), (2, 0));
    }

    #[test]
    fn negative_side_shifts_towards_the_origin() {
        let mut g = grid(&[((-3, 0), Ws("far", 1)), ((-1, 0), Ws("near", 1)), ((0, 0), Ws("a", 1))], (0, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((-2, 0), "far"), ((-1, 0), "near"), ((0, 0), "a")]);
    }

    #[test]
    fn gap_spanning_the_origin() {
        let mut g = grid(&[((-1, 0), Ws("l", 1)), ((2, 0), Ws("r", 1))], (2, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((-1, 0), "l"), ((0, 0), "r")]);
    }

    #[test]
    fn compaction_keeps_coordinates_unique() {
        // Diagonal neighbours never collide when lines are removed.
        let mut g = grid(&[((0, 0), Ws("a", 1)), ((1, 2), Ws("b", 1)), ((3, 3), Ws("c", 1))], (0, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((1, 1), "b"), ((2, 2), "c")]);
    }
}
