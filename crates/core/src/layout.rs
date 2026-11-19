//! Where a new worker goes in its project's workspace: a free pane, a new pane split off one, or a tab.
//!
//! The binary reads the workspace's panes from the host, marks the surfaces that hold a session, and acts on the
//! [`Placement`] this returns. A pane is free when none of its surfaces holds a session, so a pane the user closed
//! or rearranged by hand is simply read as it now is.
//!
//! Workers fill a grid of equal quarters, two rows of two, and no pane is ever cut smaller than a quarter of the
//! workspace. The choices follow what wmux can do without moving a surface: it splits a workspace's first pane (top
//! left) side by side or top and bottom, any other pane only side by side, and a pane whose last surface closes leaves
//! the layout, its sibling taking its space. Two rows of two is the only grid those splits reach, so the second worker
//! takes the bottom half, the third the top right quarter, the fourth the bottom right; later workers stack as tabs.
//! A closing worker's pane simply goes: no empty pane is ever cut ahead of time or kept behind, and the next worker
//! splits the half its neighbour grew into back into quarters.

use std::num::NonZeroU32;

/// The smallest share of the workspace's width and of its height a pane may have: a quarter of the workspace is half
/// its width by half its height.
pub const QUARTER_SIDE: f64 = 0.5;

/// How far a measured share may fall short of [`QUARTER_SIDE`] and still count, for the rounding of the host's ratios.
const SLACK: f64 = 1e-3;

/// One pane of a workspace, as fractions of the workspace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// Its width, from 0 to 1.
    pub width: f64,
    /// Its height, from 0 to 1.
    pub height: f64,
    /// How many surfaces (tabs) it holds.
    pub surfaces: usize,
    /// How many of them hold a session.
    pub busy: usize,
}

impl Cell {
    const fn free(&self) -> bool {
        self.busy == 0
    }

    fn area(&self) -> f64 {
        self.width * self.height
    }

    fn halves_fit(width: f64, height: f64) -> bool {
        width >= QUARTER_SIDE - SLACK && height >= QUARTER_SIDE - SLACK
    }

    fn splits_side_by_side(&self) -> bool {
        Self::halves_fit(self.width / 2.0, self.height)
    }

    fn splits_down(&self) -> bool {
        Self::halves_fit(self.width, self.height / 2.0)
    }
}

/// What to do with a new worker, by the index of a cell in the slice given to [`place`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Start it in this free pane. With `replace_tab`, it replaces the pane's only surface, an idle shell.
    Free {
        /// The cell.
        cell: usize,
        /// Replace the pane's lone shell.
        replace_tab: bool,
    },
    /// Split this pane into equal halves and start the worker in the new one (right or bottom), replacing its shell.
    Split {
        /// The cell. Only the first cell is ever split `down`, the one pane wmux splits top and bottom.
        cell: usize,
        /// Top and bottom rather than side by side.
        down: bool,
    },
    /// No pane is free and none can be split without cutting one below a quarter: add it as a tab of this one.
    Stack {
        /// The cell.
        cell: usize,
    },
}

/// Chooses a place for a new worker among `cells`, in the host's order (its first pane first, then depth first).
///
/// The first free pane wins. Without one, a workspace with fewer panes than `max_panes` gets a pane split, when one
/// can be split into two halves no smaller than a quarter: the largest such pane, a whole workspace top and bottom,
/// the earlier pane on a tie. Otherwise the worker stacks on the pane with the fewest sessions, then the largest, then
/// the later one. `None` when there are no cells. A `max_panes` above 4 acts as 4, since a fifth pane would be smaller
/// than a quarter.
#[must_use]
pub fn place(cells: &[Cell], max_panes: NonZeroU32) -> Option<Placement> {
    if let Some((cell, found)) = cells.iter().enumerate().find(|(_, c)| c.free()) {
        return Some(Placement::Free {
            cell,
            replace_tab: found.surfaces == 1,
        });
    }
    let count = u32::try_from(cells.len()).unwrap_or(u32::MAX);
    if count < max_panes.get()
        && let Some(split) = best_split(cells)
    {
        return Some(split);
    }
    cells
        .iter()
        .enumerate()
        .min_by(|(ia, a), (ib, b)| {
            a.busy
                .cmp(&b.busy)
                .then(b.area().total_cmp(&a.area()))
                .then(ib.cmp(ia))
        })
        .map(|(cell, _)| Placement::Stack { cell })
}

fn best_split(cells: &[Cell]) -> Option<Placement> {
    let down = cells
        .first()
        .filter(|first| first.splits_down())
        .map(|first| {
            (
                first.area(),
                Placement::Split {
                    cell: 0,
                    down: true,
                },
            )
        });
    let side = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.splits_side_by_side())
        .map(|(cell, c)| (c.area(), Placement::Split { cell, down: false }));
    down.into_iter()
        .chain(side)
        .reduce(|best, next| if next.0 > best.0 + SLACK { next } else { best })
        .map(|(_, split)| split)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::panic, reason = "tests")]

    use super::*;

    const fn cell(width: f64, height: f64, surfaces: usize, busy: usize) -> Cell {
        Cell {
            width,
            height,
            surfaces,
            busy,
        }
    }

    fn four() -> NonZeroU32 {
        NonZeroU32::new(4).unwrap_or(NonZeroU32::MIN)
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one table of every placement")]
    fn decisions() {
        let quarter = cell(0.5, 0.5, 1, 1);
        let row = cell(1.0, 0.5, 1, 1);
        let column = cell(0.5, 1.0, 1, 1);
        let table: &[(&str, Vec<Cell>, Option<Placement>)] = &[
            ("no cells", vec![], None),
            (
                "a fresh workspace's shell is replaced",
                vec![cell(1.0, 1.0, 1, 0)],
                Some(Placement::Free {
                    cell: 0,
                    replace_tab: true,
                }),
            ),
            (
                "a free pane with several shells gets a tab",
                vec![quarter, cell(0.5, 0.5, 2, 0), quarter, quarter],
                Some(Placement::Free {
                    cell: 1,
                    replace_tab: false,
                }),
            ),
            (
                "an empty pane gets a tab",
                vec![cell(1.0, 1.0, 0, 0)],
                Some(Placement::Free {
                    cell: 0,
                    replace_tab: false,
                }),
            ),
            (
                "the second worker splits the lone pane top and bottom",
                vec![cell(1.0, 1.0, 1, 1)],
                Some(Placement::Split {
                    cell: 0,
                    down: true,
                }),
            ),
            (
                "the third splits the top half into quarters",
                vec![row, row],
                Some(Placement::Split {
                    cell: 0,
                    down: false,
                }),
            ),
            (
                "the fourth splits the bottom half into quarters",
                vec![quarter, quarter, row],
                Some(Placement::Split {
                    cell: 2,
                    down: false,
                }),
            ),
            (
                "a top row's worker gone, the half it left is split again",
                vec![row, quarter, quarter],
                Some(Placement::Split {
                    cell: 0,
                    down: false,
                }),
            ),
            (
                "two columns left by a row that closed: the first is split top and bottom",
                vec![column, column],
                Some(Placement::Split {
                    cell: 0,
                    down: true,
                }),
            ),
            (
                "a right column is never cut into narrow strips",
                vec![quarter, quarter, column],
                Some(Placement::Stack { cell: 2 }),
            ),
            (
                "a quarter is never split, the fifth stacks on the last of the least busy",
                vec![cell(0.5, 0.5, 2, 2), quarter, quarter, cell(0.5, 0.5, 2, 2)],
                Some(Placement::Stack { cell: 2 }),
            ),
            (
                "an uneven workspace stacks on its largest least busy pane",
                vec![
                    cell(0.25, 0.5, 1, 1),
                    cell(0.25, 0.5, 1, 1),
                    quarter,
                    cell(0.5, 1.0, 2, 2),
                ],
                Some(Placement::Stack { cell: 2 }),
            ),
            (
                "a lone eighth beside a quarter is not split",
                vec![cell(0.25, 0.5, 1, 1), quarter, cell(0.5, 1.0, 2, 2)],
                Some(Placement::Stack { cell: 1 }),
            ),
            (
                "panes the user made beyond the limit are not split",
                vec![cell(0.2, 1.0, 1, 1); 5],
                Some(Placement::Stack { cell: 4 }),
            ),
        ];
        for (name, cells, want) in table {
            assert_eq!(place(cells, four()), *want, "{name}");
        }
    }

    #[test]
    fn one_pane_means_tabs() {
        assert_eq!(
            place(&[cell(1.0, 1.0, 1, 1)], NonZeroU32::MIN),
            Some(Placement::Stack { cell: 0 }),
            "no split with a limit of one"
        );
    }

    #[test]
    fn a_limit_above_four_still_stops_at_quarters() {
        let quarters = vec![cell(0.5, 0.5, 1, 1); 4];
        assert_eq!(
            place(&quarters, NonZeroU32::new(8).unwrap()),
            Some(Placement::Stack { cell: 3 }),
            "no fifth pane"
        );
    }

    /// A workspace as wmux keeps it: a binary tree of splits whose leaves are panes holding workers, or the fresh
    /// workspace's shell.
    #[derive(Debug, Clone)]
    enum Tree {
        Leaf { workers: Vec<u32>, shell: bool },
        Split { down: bool, parts: Box<[Self; 2]> },
    }

    impl Tree {
        fn fresh() -> Self {
            Self::Leaf {
                workers: Vec::new(),
                shell: true,
            }
        }

        fn cells(&self) -> Vec<(Cell, Vec<u32>)> {
            let mut out = Vec::new();
            self.collect(1.0, 1.0, &mut out);
            out
        }

        fn collect(&self, width: f64, height: f64, out: &mut Vec<(Cell, Vec<u32>)>) {
            match self {
                Self::Leaf { workers, shell } => out.push((
                    cell(
                        width,
                        height,
                        workers.len() + usize::from(*shell),
                        workers.len(),
                    ),
                    workers.clone(),
                )),
                Self::Split { down, parts } => {
                    let (w, h) = if *down {
                        (width, height / 2.0)
                    } else {
                        (width / 2.0, height)
                    };
                    for part in &**parts {
                        part.collect(w, h, out);
                    }
                }
            }
        }

        fn leaf(&mut self, index: usize) -> &mut Self {
            fn walk<'a>(
                tree: &'a mut Tree,
                index: usize,
                seen: &mut usize,
            ) -> Option<&'a mut Tree> {
                match tree {
                    Tree::Leaf { .. } => {
                        *seen += 1;
                        (*seen - 1 == index).then_some(tree)
                    }
                    Tree::Split { parts, .. } => {
                        let [a, b] = &mut **parts;
                        walk(a, index, seen).or_else(|| walk(b, index, seen))
                    }
                }
            }
            walk(self, index, &mut 0).unwrap()
        }

        fn open(&mut self, worker: u32) {
            let cells: Vec<Cell> = self.cells().into_iter().map(|(c, _)| c).collect();
            match place(&cells, four()).unwrap() {
                Placement::Free { cell, .. } => {
                    *self.leaf(cell) = Self::Leaf {
                        workers: vec![worker],
                        shell: false,
                    };
                }
                Placement::Split { cell, down } => {
                    assert!(
                        !down || cell == 0,
                        "only the first pane splits top and bottom"
                    );
                    let leaf = self.leaf(cell);
                    let old = std::mem::replace(leaf, Self::fresh());
                    let new = Self::Leaf {
                        workers: vec![worker],
                        shell: false,
                    };
                    *leaf = Self::Split {
                        down,
                        parts: Box::new([old, new]),
                    };
                }
                Placement::Stack { cell } => match self.leaf(cell) {
                    Self::Leaf { workers, .. } => workers.push(worker),
                    Self::Split { .. } => panic!("a leaf"),
                },
            }
        }

        /// Closes a worker's surface; its pane leaves when it was the last surface, its sibling taking the space.
        /// `None` when the workspace closes with it.
        fn close(self, worker: u32) -> Option<Self> {
            match self {
                Self::Leaf { mut workers, shell } => {
                    workers.retain(|w| *w != worker);
                    (!workers.is_empty() || shell).then_some(Self::Leaf { workers, shell })
                }
                Self::Split { down, parts } => {
                    let [a, b] = *parts;
                    match (a.close(worker), b.close(worker)) {
                        (Some(a), Some(b)) => Some(Self::Split {
                            down,
                            parts: Box::new([a, b]),
                        }),
                        (Some(one), None) | (None, Some(one)) => Some(one),
                        (None, None) => None,
                    }
                }
            }
        }
    }

    type Shape = (f64, f64, usize);

    fn shape(tree: &Tree) -> Vec<Shape> {
        tree.cells()
            .into_iter()
            .map(|(c, _)| (c.width, c.height, c.busy))
            .collect()
    }

    fn assert_sound(tree: &Tree, context: &str) {
        let cells = tree.cells();
        assert!(
            cells.len() <= 4,
            "at most four panes, {context}: {:?}",
            shape(tree)
        );
        assert!(
            cells
                .iter()
                .all(|(c, _)| c.width >= QUARTER_SIDE - SLACK && c.height >= QUARTER_SIDE - SLACK),
            "no pane smaller than a quarter, {context}: {:?}",
            shape(tree)
        );
    }

    fn quarters(tree: &Tree) -> bool {
        let cells = tree.cells();
        cells.len() == 4
            && cells
                .iter()
                .all(|(c, _)| (c.width - 0.5).abs() < SLACK && (c.height - 0.5).abs() < SLACK)
    }

    fn opened(count: u32) -> Tree {
        let mut tree = Tree::fresh();
        for worker in 1..=count {
            tree.open(worker);
        }
        tree
    }

    #[test]
    fn one_to_six_workers_fill_the_grid_then_stack() {
        let table: [(u32, &[Shape]); 6] = [
            (1, &[(1.0, 1.0, 1)]),
            (2, &[(1.0, 0.5, 1), (1.0, 0.5, 1)]),
            (3, &[(0.5, 0.5, 1), (0.5, 0.5, 1), (1.0, 0.5, 1)]),
            (4, &[(0.5, 0.5, 1); 4]),
            (
                5,
                &[(0.5, 0.5, 1), (0.5, 0.5, 1), (0.5, 0.5, 1), (0.5, 0.5, 2)],
            ),
            (
                6,
                &[(0.5, 0.5, 1), (0.5, 0.5, 1), (0.5, 0.5, 2), (0.5, 0.5, 2)],
            ),
        ];
        for (count, want) in table {
            assert_eq!(shape(&opened(count)), want, "{count} workers");
        }
    }

    fn permutations(items: &[u32]) -> Vec<Vec<u32>> {
        if items.len() <= 1 {
            return vec![items.to_vec()];
        }
        let mut out = Vec::new();
        for (i, first) in items.iter().enumerate() {
            let mut rest = items.to_vec();
            rest.remove(i);
            for mut tail in permutations(&rest) {
                tail.insert(0, *first);
                out.push(tail);
            }
        }
        out
    }

    #[test]
    fn any_one_close_in_the_grid_is_filled_back_into_quarters() {
        for gone in 1..=4 {
            let tree = opened(4).close(gone).unwrap();
            assert_sound(&tree, &format!("after {gone} closed"));
            let mut tree = tree;
            tree.open(9);
            assert!(
                quarters(&tree),
                "{gone} closed, the next worker restores the grid: {:?}",
                shape(&tree)
            );
        }
    }

    #[test]
    fn a_close_with_tabs_stacked_is_filled_back_into_quarters() {
        for gone in 1..=6 {
            let mut tree = opened(6).close(gone).unwrap();
            tree.open(9);
            assert_sound(&tree, &format!("six, {gone} closed, one more"));
            assert_eq!(
                tree.cells().len(),
                4,
                "{gone} closed: four panes again: {:?}",
                shape(&tree)
            );
        }
    }

    #[test]
    fn closes_in_any_order_never_cut_a_pane_below_a_quarter() {
        for order in permutations(&[1, 2, 3, 4, 5, 6]) {
            let mut tree = Some(opened(6));
            let mut next = 10;
            for (step, worker) in order.iter().enumerate() {
                tree = tree.and_then(|t| t.close(*worker));
                let Some(open) = tree.as_mut() else {
                    assert_eq!(
                        step, 5,
                        "the workspace closes with its last worker, {order:?}"
                    );
                    break;
                };
                assert_sound(open, &format!("{order:?} after {step}"));
                let mut refilled = open.clone();
                for _ in 0..3 {
                    refilled.open(next);
                    next += 1;
                    assert_sound(&refilled, &format!("{order:?} after {step}, refilled"));
                }
            }
        }
    }

    #[test]
    fn a_workspace_that_empties_starts_the_grid_over() {
        let mut tree = opened(4);
        for worker in [1, 3] {
            tree = tree.close(worker).unwrap();
        }
        assert_eq!(
            shape(&tree),
            [(0.5, 1.0, 1), (0.5, 1.0, 1)],
            "the bottom row grew into two columns"
        );
        tree.open(5);
        assert_eq!(
            shape(&tree),
            [(0.5, 0.5, 1), (0.5, 0.5, 1), (0.5, 1.0, 1)],
            "the left column splits top and bottom; wmux cannot split the right one so"
        );
        tree.open(6);
        assert_eq!(
            tree.cells().len(),
            3,
            "the sixth stacks rather than cut the right column into strips"
        );
        let mut fresh = Tree::fresh();
        for worker in 1..=4 {
            fresh.open(worker);
        }
        assert!(
            quarters(&fresh),
            "an emptied workspace starts over as a grid"
        );
    }
}
