//! Turn a split tree into cell rectangles.
//!
//! Sizes are in character cells, like tmux: each pane gets whole cells and
//! adjacent children are separated by a one-cell divider. Every client draws
//! the same rectangles, which are exactly the panes' PTY sizes.

use serde::{Deserialize, Serialize};

use crate::{
    NodeId, PaneId,
    tree::{Dir, Node},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

/// A split's area and how long each child is along the split's direction,
/// so a client can turn a divider drag into new weights.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SplitRect {
    pub id: NodeId,
    pub dir: Dir,
    pub rect: Rect,
    pub extents: Vec<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Layout {
    pub panes: Vec<(PaneId, Rect)>,
    pub splits: Vec<SplitRect>,
}

pub fn layout(root: &Node, cols: u16, rows: u16) -> Layout {
    let mut out = Layout::default();
    place(root, Rect { x: 0, y: 0, cols: cols.max(1), rows: rows.max(1) }, &mut out);
    out
}

fn place(node: &Node, rect: Rect, out: &mut Layout) {
    match node {
        Node::Pane { pane } => out.panes.push((*pane, rect)),
        Node::Split { id, dir, children } => {
            let total = match dir {
                Dir::Row => rect.cols,
                Dir::Column => rect.rows,
            };
            let weights: Vec<f64> = children.iter().map(|c| c.weight).collect();
            let mins: Vec<u16> = children
                .iter()
                .map(|c| {
                    let (cols, rows) = c.node.min_size();
                    if *dir == Dir::Row { cols } else { rows }
                })
                .collect();
            let extents = distribute_min(total, &weights, &mins);
            out.splits.push(SplitRect { id: *id, dir: *dir, rect, extents: extents.clone() });
            let mut at = 0u16;
            for (child, len) in children.iter().zip(extents) {
                let r = match dir {
                    Dir::Row => Rect { x: rect.x + at, y: rect.y, cols: len, rows: rect.rows },
                    Dir::Column => Rect { x: rect.x, y: rect.y + at, cols: rect.cols, rows: len },
                };
                place(&child.node, r, out);
                at = at.saturating_add(len).saturating_add(1);
            }
        }
    }
}

/// Split `total` cells among children by weight, leaving one cell between
/// neighbours. Every child gets at least one cell (if there are too few
/// cells, the last children overflow and get clipped by the client).
pub fn distribute(total: u16, weights: &[f64]) -> Vec<u16> {
    let n = weights.len();
    if n == 0 {
        return vec![];
    }
    let avail = (total as usize).saturating_sub(n - 1).max(n);
    let sum: f64 = weights.iter().sum();
    // Weights taken from cell extents (a tmux layout) must give those cells
    // back: 2/103 * 103 is 1.999..., so snap what is an integer but for
    // rounding.
    let snap = |e: f64| if (e - e.round()).abs() < 1e-9 { e.round() } else { e };
    let exact: Vec<f64> = weights.iter().map(|w| snap(w / sum * avail as f64)).collect();
    let mut sizes: Vec<usize> = exact.iter().map(|e| (e.floor() as usize).max(1)).collect();
    // Hand out what's left by largest remainder; take back any excess from
    // the largest children.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| (exact[b] - exact[b].floor()).total_cmp(&(exact[a] - exact[a].floor())));
    let mut used: usize = sizes.iter().sum();
    let mut i = 0;
    while used < avail {
        sizes[order[i % n]] += 1;
        used += 1;
        i += 1;
    }
    while used > avail {
        let (big, _) = sizes.iter().enumerate().max_by_key(|(_, s)| **s).unwrap();
        if sizes[big] <= 1 {
            break;
        }
        sizes[big] -= 1;
        used -= 1;
    }
    sizes.into_iter().map(|s| s.min(u16::MAX as usize) as u16).collect()
}

/// [`distribute`], then cells moved to any child that got fewer than its
/// minimum (a nested split needs room for its own panes and dividers), one
/// at a time from the child with the most to spare. Sizes that already meet
/// the minimums come out unchanged, so weights derived from cells (a tmux
/// layout) reproduce those cells exactly.
pub fn distribute_min(total: u16, weights: &[f64], mins: &[u16]) -> Vec<u16> {
    let mut sizes = distribute(total, weights);
    loop {
        let Some(short) = (0..sizes.len()).find(|&i| sizes[i] < mins.get(i).copied().unwrap_or(1)) else {
            return sizes;
        };
        let spare = |i: usize| sizes[i] as i32 - mins.get(i).copied().unwrap_or(1) as i32;
        let Some(donor) = (0..sizes.len()).filter(|&i| spare(i) > 0).max_by_key(|&i| spare(i)) else {
            return sizes;
        };
        sizes[donor] -= 1;
        sizes[short] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Edge;

    #[test]
    fn distribute_fills_exactly() {
        assert_eq!(distribute(81, &[0.5, 0.5]), vec![40, 40]);
        assert_eq!(distribute(80, &[0.5, 0.5]), vec![40, 39]);
        assert_eq!(distribute(100, &[0.5, 0.25, 0.25]), vec![49, 25, 24]);
        assert_eq!(distribute(3, &[0.5, 0.5]), vec![1, 1]);
        assert_eq!(distribute(1, &[0.5, 0.5]), vec![1, 1]);
        for total in 5..200 {
            let s = distribute(total, &[0.3, 0.3, 0.4]);
            assert_eq!(s.iter().sum::<u16>() + 2, total, "total {total}: {s:?}");
        }
    }

    #[test]
    fn nested_splits_get_room_for_their_panes() {
        // A thin column holding a row of three panes needs 5 cells.
        assert_eq!(distribute_min(20, &[0.9, 0.1], &[1, 5]), vec![14, 5]);
        // Already enough: unchanged.
        assert_eq!(distribute_min(20, &[0.5, 0.5], &[1, 5]), distribute(20, &[0.5, 0.5]));
        let mut n = 0;
        let mut next = || {
            n += 1;
            n
        };
        let mut t = Node::pane(1);
        t.insert(1, Edge::Right, Node::pane(2), &mut next);
        t.insert(2, Edge::Bottom, Node::pane(3), &mut next);
        t.insert(3, Edge::Right, Node::pane(4), &mut next);
        t.insert(4, Edge::Right, Node::pane(5), &mut next);
        assert_eq!(t.min_size(), (7, 3));
        let l = layout(&t, 7, 3);
        for (_, r) in &l.panes {
            assert!(r.x + r.cols <= 7 && r.y + r.rows <= 3, "{:?}", l.panes);
        }
    }

    #[test]
    fn panes_tile_the_area_with_one_cell_dividers() {
        let mut n = 0;
        let mut next = || {
            n += 1;
            n
        };
        let mut t = Node::pane(1);
        t.insert(1, Edge::Right, Node::pane(2), &mut next);
        t.insert(2, Edge::Bottom, Node::pane(3), &mut next);
        let l = layout(&t, 81, 25);
        assert_eq!(
            l.panes,
            vec![
                (1, Rect { x: 0, y: 0, cols: 40, rows: 25 }),
                (2, Rect { x: 41, y: 0, cols: 40, rows: 12 }),
                (3, Rect { x: 41, y: 13, cols: 40, rows: 12 }),
            ]
        );
        assert_eq!(l.splits.len(), 2);
        assert_eq!(l.splits[0].extents, vec![40, 40]);
    }
}
