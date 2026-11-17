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

use std::collections::{BTreeSet, HashMap, HashSet};

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
        self.target_from(self.current, dir, active)
    }

    /// Like [`Grid::target`], but from any workspace (a missing one counts as inactive).
    pub fn target_from(&self, from: Coord, dir: Dir, active: impl Fn(&W) -> bool) -> Option<Coord> {
        let to = neighbour(from, dir);
        let from_active = self.cells.get(&from).is_some_and(&active);
        let to_active = self.cells.get(&to).is_some_and(&active);
        (from_active || to_active).then_some(to)
    }

    /// Add a workspace at `c` unless one is there already.
    pub fn insert(&mut self, c: Coord, make: impl FnOnce() -> W) {
        self.cells.entry(c).or_insert_with(make);
    }

    /// Empty cells next to active workspaces: where a new workspace could go.
    pub fn slots(&self, active: impl Fn(&W) -> bool) -> Vec<Coord> {
        let mut out: Vec<Coord> = Vec::new();
        for (c, w) in &self.cells {
            if !active(w) {
                continue;
            }
            for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
                let n = neighbour(*c, dir);
                if !self.cells.contains_key(&n) && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out.sort();
        out
    }

    /// Make `to` current, creating an empty workspace there if needed.
    pub fn switch_to(&mut self, to: Coord, make: impl FnOnce() -> W) {
        self.cells.entry(to).or_insert_with(make);
        self.current = to;
    }

    /// Drop inactive workspaces (except the current one), close up empty rows and
    /// columns, keep the origin on an active workspace (or the only one), and slide
    /// islands of active workspaces towards the origin (along y, then x) until they
    /// join the main group. Returns the dropped workspaces.
    pub fn compact(&mut self, active: impl Fn(&W) -> bool) -> Vec<W> {
        let current = self.current;
        let dead: Vec<Coord> =
            self.cells.iter().filter(|(c, w)| **c != current && !active(w)).map(|(c, _)| *c).collect();
        let dropped = dead.iter().filter_map(|c| self.cells.remove(c)).collect();

        // Each step only moves workspaces towards the origin, so this settles quickly;
        // the cap is a safety net.
        for _ in 0..64 {
            let mut changed = self.close_gaps();
            changed |= self.anchor_origin(&active);
            changed |= self.join_islands(&active);
            if !changed {
                break;
            }
        }
        dropped
    }

    /// Remove rows/columns that no workspace uses. Returns whether anything moved.
    fn close_gaps(&mut self) -> bool {
        let xs: BTreeSet<i32> = self.cells.keys().map(|c| c.0).collect();
        let ys: BTreeSet<i32> = self.cells.keys().map(|c| c.1).collect();
        let (gx, gy) = (gaps(&xs), gaps(&ys));
        if gx.is_empty() && gy.is_empty() {
            return false;
        }
        let remap = |(x, y): Coord| (close_gaps(x, &gx), close_gaps(y, &gy));
        self.cells = std::mem::take(&mut self.cells).into_iter().map(|(c, w)| (remap(c), w)).collect();
        self.current = remap(self.current);
        true
    }

    fn translate(&mut self, (dx, dy): Coord) {
        self.cells = std::mem::take(&mut self.cells).into_iter().map(|((x, y), w)| ((x + dx, y + dy), w)).collect();
        self.current = (self.current.0 + dx, self.current.1 + dy);
    }

    fn active_cells(&self, active: &impl Fn(&W) -> bool) -> HashSet<Coord> {
        self.cells.iter().filter(|(_, w)| active(w)).map(|(c, _)| *c).collect()
    }

    /// The origin holds either the only workspace, or a non-empty one: otherwise the
    /// grid shifts so the active workspace nearest the origin (preferring bigger
    /// groups) sits there.
    fn anchor_origin(&mut self, active: &impl Fn(&W) -> bool) -> bool {
        let act = self.active_cells(active);
        if act.is_empty() {
            // Only the (empty) current workspace is left.
            let only = self.current;
            if self.cells.len() == 1 && only != (0, 0) {
                self.translate((-only.0, -only.1));
                return true;
            }
            return false;
        }
        if act.contains(&(0, 0)) {
            return false;
        }
        let groups = components(&act);
        let size = |c: &Coord| groups.iter().find(|g| g.contains(c)).map_or(0, Vec::len);
        let target = *act
            .iter()
            .min_by_key(|c| (c.0.abs() + c.1.abs(), std::cmp::Reverse(size(c)), c.1.abs(), c.0.abs(), c.1, c.0))
            .expect("not empty");
        self.translate((-target.0, -target.1));
        true
    }

    /// Islands (groups of active workspaces not connected to the one at the origin)
    /// slide towards the origin along y until they touch the main group; failing that,
    /// along x. Moves one island per call; returns whether anything moved.
    fn join_islands(&mut self, active: &impl Fn(&W) -> bool) -> bool {
        let act = self.active_cells(active);
        let groups = components(&act);
        let Some(main) = groups.iter().find(|g| g.contains(&(0, 0))) else { return false };
        let main: HashSet<Coord> = main.iter().copied().collect();
        let mut islands: Vec<&Vec<Coord>> = groups.iter().filter(|g| !g.contains(&(0, 0))).collect();
        islands.sort_by_key(|g| g.iter().map(|c| c.0.abs() + c.1.abs()).min());

        for island in islands {
            let others: HashSet<Coord> = self.cells.keys().filter(|c| !island.contains(c)).copied().collect();
            let Some(offset) = slide(island, &others, &main) else { continue };
            let moved: Vec<(Coord, W)> = island.iter().filter_map(|c| self.cells.remove(c).map(|w| (*c, w))).collect();
            for ((x, y), w) in moved {
                if self.current == (x, y) {
                    self.current = (x + offset.0, y + offset.1);
                }
                self.cells.insert((x + offset.0, y + offset.1), w);
            }
            return true;
        }
        false
    }
}

fn neighbours((x, y): Coord) -> [Coord; 4] {
    [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
}

/// Groups of 4-connected coordinates.
fn components(cells: &HashSet<Coord>) -> Vec<Vec<Coord>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut sorted: Vec<Coord> = cells.iter().copied().collect();
    sorted.sort();
    for start in sorted {
        if !seen.insert(start) {
            continue;
        }
        let mut group = vec![start];
        let mut i = 0;
        while i < group.len() {
            for n in neighbours(group[i]) {
                if cells.contains(&n) && seen.insert(n) {
                    group.push(n);
                }
            }
            i += 1;
        }
        out.push(group);
    }
    out
}

/// How far to move `island` (towards the origin, along y and then x) so it touches
/// `main`, stopping early if something (`occupied`) is in the way. `None` if it can't
/// move at all.
fn slide(island: &[Coord], occupied: &HashSet<Coord>, main: &HashSet<Coord>) -> Option<Coord> {
    let at = |off: Coord| island.iter().map(move |c| (c.0 + off.0, c.1 + off.1));
    let touches = |off: Coord| at(off).any(|c| neighbours(c).iter().any(|n| main.contains(n)));
    let blocked = |off: Coord| at(off).any(|c| occupied.contains(&c));

    let mut off = (0, 0);
    for axis in [1, 0] {
        loop {
            let (lo, hi) = at(off).fold((i32::MAX, i32::MIN), |(lo, hi), c| {
                let v = if axis == 1 { c.1 } else { c.0 };
                (lo.min(v), hi.max(v))
            });
            // Towards zero; an island already spanning the axis stays put on it.
            let step = if lo > 0 {
                -1
            } else if hi < 0 {
                1
            } else {
                break;
            };
            let next = if axis == 1 { (off.0, off.1 + step) } else { (off.0 + step, off.1) };
            if blocked(next) {
                break;
            }
            off = next;
            if touches(off) {
                return Some(off);
            }
        }
    }
    (off != (0, 0)).then_some(off)
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
        let mut g = grid(
            &[
                ((0, 0), Ws("a", 1)),
                ((1, 0), Ws("f", 1)),
                ((0, 1), Ws("b", 0)),
                ((1, 1), Ws("e", 1)),
                ((0, 2), Ws("c", 1)),
                ((1, 2), Ws("g", 1)),
            ],
            (0, 2),
        );
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((0, 2), "c"), ((1, 0), "f"), ((1, 1), "e"), ((1, 2), "g")]);
    }

    /// Workspaces that only touch diagonally are separate groups: the lower one slides
    /// up next to the other.
    #[test]
    fn diagonal_neighbours_join_up() {
        let mut g =
            grid(&[((0, 0), Ws("a", 1)), ((0, 1), Ws("b", 0)), ((1, 1), Ws("e", 1)), ((0, 2), Ws("c", 1))], (0, 2));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((0, 1), "c"), ((1, 1), "e")]);
        assert_eq!(g.current(), (0, 1), "still on c");
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
    fn slots_surround_active_workspaces() {
        let g = grid(&[((0, 0), Ws("a", 1)), ((1, 0), Ws("empty", 0))], (1, 0));
        // Around (0,0), minus the existing (1,0); the empty workspace has no slots of its own.
        assert_eq!(g.slots(active), vec![(-1, 0), (0, -1), (0, 1)]);
    }

    #[test]
    fn insert_keeps_existing() {
        let mut g = grid(&[((0, 0), Ws("a", 1))], (0, 0));
        g.insert((0, 0), || Ws("new", 0));
        g.insert((0, 1), || Ws("b", 0));
        assert_eq!(named(&g), vec![((0, 0), "a"), ((0, 1), "b")]);
    }

    #[test]
    fn compaction_keeps_coordinates_unique() {
        // Nothing collides when lines are removed and islands slide in.
        let mut g = grid(&[((0, 0), Ws("a", 1)), ((1, 2), Ws("b", 1)), ((3, 3), Ws("c", 1))], (0, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((1, 0), "b"), ((2, 0), "c")]);
    }

    fn cross() -> Vec<(Coord, Ws)> {
        let mut cells = vec![((0, 0), Ws("o", 1))];
        for v in [-3, -2, -1, 1, 2, 3] {
            cells.push(((v, 0), Ws("row", 1)));
            cells.push(((0, v), Ws("col", 1)));
        }
        cells
    }

    /// The spec's stranded island: a full cross through the origin, plus (3,3),(4,3).
    /// The island slides up along y until it touches the cross: (3,1),(4,1).
    #[test]
    fn stranded_island_slides_along_y_to_the_group() {
        let mut cells = cross();
        cells.push(((3, 3), Ws("i1", 1)));
        cells.push(((4, 3), Ws("i2", 1)));
        let mut g = grid(&cells, (0, 0));
        g.compact(active);
        let at = |name| named(&g).into_iter().find(|(_, n)| *n == name).unwrap().0;
        assert_eq!((at("i1"), at("i2")), ((3, 1), (4, 1)));
    }

    #[test]
    fn island_above_the_origin_moves_down_towards_it() {
        let mut cells = cross();
        cells.push(((-2, -3), Ws("i", 1)));
        let mut g = grid(&cells, (0, 0));
        g.compact(active);
        assert!(named(&g).contains(&((-2, -1), "i")));
    }

    /// Blocked on y (by the current, empty workspace), it continues along x.
    #[test]
    fn island_blocked_on_y_slides_along_x() {
        let mut g = grid(
            &[
                ((0, 0), Ws("a", 1)),
                ((0, 1), Ws("b", 1)),
                ((2, 1), Ws("here", 0)),
                ((2, 2), Ws("i", 1)),
                ((1, 2), Ws("c", 1)),
            ],
            (2, 1),
        );
        // "i" touches "c" already; move "c" out of the way to make it an island.
        g.cells.remove(&(1, 2));
        g.cells.insert((0, 2), Ws("c", 1));
        g.compact(active);
        // Columns: 0, 2 -> close gap -> 0, 1. "i" can't go up (here), slides left to x=1
        // next to "c" at (0,2).
        assert!(named(&g).contains(&((1, 2), "i")), "{:?}", named(&g));
        assert_eq!(g.current(), (1, 1));
    }

    #[test]
    fn empty_origin_moves_to_the_nearest_active_workspace() {
        let mut g = grid(&[((0, 0), Ws("o", 0)), ((1, 0), Ws("a", 1)), ((1, 1), Ws("b", 1))], (1, 1));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "a"), ((0, 1), "b")]);
        assert_eq!(g.current(), (0, 1));
    }

    #[test]
    fn current_empty_origin_shifts_if_others_are_active() {
        let mut g = grid(&[((0, 0), Ws("here", 0)), ((1, 0), Ws("a", 1))], (0, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((-1, 0), "here"), ((0, 0), "a")]);
        assert_eq!(g.current(), (-1, 0));
    }

    #[test]
    fn lone_workspace_is_the_origin() {
        let mut g = grid(&[((0, 0), Ws("a", 1)), ((2, 3), Ws("here", 0))], (2, 3));
        g.cells.remove(&(0, 0));
        g.compact(active);
        assert_eq!(named(&g), vec![((0, 0), "here")]);
    }

    /// After compaction, every active workspace is connected to the origin (for random
    /// layouts that have room to slide).
    #[test]
    fn compaction_leaves_no_islands() {
        let mut seed = 0x2545_f491_u64;
        let mut rand = move |n: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as i32
        };
        for _ in 0..500 {
            let mut cells = HashMap::new();
            for i in 0..rand(8) + 1 {
                cells.insert((rand(9) - 4, rand(9) - 4), Ws(["a", "b", "c", "d", "e", "f", "g", "h"][i as usize], 1));
            }
            let current = *cells.keys().next().unwrap();
            let mut g = Grid { cells, current };
            g.compact(active);
            let act = g.active_cells(&active);
            assert!(act.contains(&(0, 0)), "origin active: {:?}", named(&g));
            let groups = components(&act);
            assert_eq!(groups.len(), 1, "one group: {:?}", named(&g));
        }
    }
}
