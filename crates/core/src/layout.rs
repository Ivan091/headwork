//! Where a new child goes in its project's workspace: a free pane, a grid made of a lone pane, or a tab.
//!
//! The binary reads the workspace's panes from the host, marks the surfaces that hold a session, and acts on the
//! [`Placement`] this returns. A pane is free when none of its surfaces holds a session, so a pane the user closed
//! or rearranged by hand is simply read as it now is.
//!
//! The choices follow what wmux can do without moving a surface: it can cut a workspace that holds one pane into a
//! grid of any size, and split a workspace's first pane in two; it cannot split any other pane. So the second child
//! turns the lone pane into the whole grid at once, and a closing child leaves its pane behind for the next one.

use std::num::NonZeroU32;

/// The width of the host window over its height. Only the direction of a split depends on it: a pane wider than tall
/// is split side by side, one taller than wide top and bottom.
pub const WINDOW_ASPECT: f64 = 16.0 / 9.0;

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

    fn taller_than_wide(&self) -> bool {
        self.height > self.width * WINDOW_ASPECT
    }
}

/// What to do with a new child, by the index of a cell in the slice given to [`place`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Start it in this free pane. With `replace_tab`, it replaces the pane's only surface, an idle shell.
    Free {
        /// The cell.
        cell: usize,
        /// Replace the pane's lone shell.
        replace_tab: bool,
    },
    /// Cut this lone pane into a grid of `count` panes, then start it in the first new one, replacing its shell.
    Grid {
        /// The cell.
        cell: usize,
        /// How many panes the grid has, the cut one included.
        count: NonZeroU32,
    },
    /// Split the workspace's first pane and start it in the new half, replacing its shell. Only when a pane the grid
    /// had is gone, closed by hand.
    Split {
        /// Top and bottom rather than side by side.
        down: bool,
    },
    /// Every pane holds a session and the workspace has its most panes: add it as a tab of this one.
    Stack {
        /// The cell.
        cell: usize,
    },
}

/// Chooses a place for a new child among `cells`, in the host's order (left to right, top to bottom).
///
/// The first free pane wins. Without one, a lone pane becomes a grid of `max_panes`, and a workspace with fewer
/// panes than that gets its first pane split. Otherwise the child stacks on the pane with the fewest sessions, the
/// later pane on a tie. `None` when there are no cells.
#[must_use]
pub fn place(cells: &[Cell], max_panes: NonZeroU32) -> Option<Placement> {
    if let Some((cell, found)) = cells.iter().enumerate().find(|(_, c)| c.free()) {
        return Some(Placement::Free {
            cell,
            replace_tab: found.surfaces == 1,
        });
    }
    let count = u32::try_from(cells.len()).unwrap_or(u32::MAX);
    if count < max_panes.get() {
        if let [_] = cells {
            return Some(Placement::Grid {
                cell: 0,
                count: max_panes,
            });
        }
        if let Some(first) = cells.first() {
            return Some(Placement::Split {
                down: first.taller_than_wide(),
            });
        }
    }
    cells
        .iter()
        .enumerate()
        .min_by(|(ia, a), (ib, b)| a.busy.cmp(&b.busy).then(ib.cmp(ia)))
        .map(|(cell, _)| Placement::Stack { cell })
}

#[cfg(test)]
mod tests {
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
    fn decisions() {
        let quarter = cell(0.5, 0.5, 1, 1);
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
                "the second child makes the grid",
                vec![cell(1.0, 1.0, 1, 1)],
                Some(Placement::Grid {
                    cell: 0,
                    count: four(),
                }),
            ),
            (
                "a pane freed by a closed child is reused",
                vec![quarter, cell(0.5, 0.5, 2, 0), quarter, quarter],
                Some(Placement::Free {
                    cell: 1,
                    replace_tab: false,
                }),
            ),
            (
                "a grid short of a pane closed by hand splits the first pane",
                vec![cell(0.5, 1.0, 1, 1), quarter, quarter],
                Some(Placement::Split { down: true }),
            ),
            (
                "the fifth stacks on the last of the least busy",
                vec![cell(0.5, 0.5, 2, 2), quarter, quarter, cell(0.5, 0.5, 2, 2)],
                Some(Placement::Stack { cell: 2 }),
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
            "no grid with a limit of one"
        );
    }

    #[test]
    fn a_wide_first_pane_splits_side_by_side() {
        assert_eq!(
            place(&[cell(1.0, 0.5, 1, 1), cell(1.0, 0.5, 1, 1)], four()),
            Some(Placement::Split { down: false }),
            "wider than tall"
        );
    }
}
