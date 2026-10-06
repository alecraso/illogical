//! A tab's split tree.
//!
//! Leaves are panes; inner nodes split their area into a row (side by side)
//! or a column (stacked) of children, each with a weight. The tree is kept
//! normalized: every split has at least two children, weights are positive
//! and sum to 1, and no split has a child split in the same direction
//! (that child's children are pulled up into it instead).

use serde::{Deserialize, Serialize};

use crate::{NodeId, PaneId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Dir {
    /// Children side by side, left to right.
    Row,
    /// Children stacked, top to bottom.
    Column,
}

/// Where to put something relative to a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    /// Take the pane's place (swap with it).
    Center,
}

impl Edge {
    pub fn dir(self) -> Option<Dir> {
        match self {
            Edge::Left | Edge::Right => Some(Dir::Row),
            Edge::Top | Edge::Bottom => Some(Dir::Column),
            Edge::Center => None,
        }
    }

    /// Whether the new node goes after the target (right or below).
    fn after(self) -> bool {
        matches!(self, Edge::Right | Edge::Bottom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Node {
    Pane { pane: PaneId },
    Split { id: NodeId, dir: Dir, children: Vec<Child> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Child {
    pub weight: f64,
    pub node: Node,
}

impl Node {
    pub fn pane(pane: PaneId) -> Self {
        Node::Pane { pane }
    }

    /// Panes in reading order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane { pane } => out.push(*pane),
            Node::Split { children, .. } => children.iter().for_each(|c| c.node.collect(out)),
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane { pane: p } => *p == pane,
            Node::Split { children, .. } => children.iter().any(|c| c.node.contains(pane)),
        }
    }

    /// The tree without `pane`, or `None` if nothing is left.
    pub fn remove(self, pane: PaneId) -> Option<Node> {
        match self {
            Node::Pane { pane: p } => (p != pane).then_some(self),
            Node::Split { id, dir, children } => {
                let children: Vec<Child> = children
                    .into_iter()
                    .filter_map(|c| c.node.remove(pane).map(|node| Child { weight: c.weight, node }))
                    .collect();
                let mut node = match children.len() {
                    0 => return None,
                    1 => children.into_iter().next().unwrap().node,
                    _ => Node::Split { id, dir, children },
                };
                node.normalize();
                Some(node)
            }
        }
    }

    /// Put `node` beside `target`, on the side `edge` names. Returns false if
    /// `target` is not in the tree or `edge` is [`Edge::Center`] (a swap,
    /// which callers do with [`Node::replace_pane`]).
    pub fn insert(&mut self, target: PaneId, edge: Edge, node: Node, next_id: &mut impl FnMut() -> NodeId) -> bool {
        let Some(dir) = edge.dir() else { return false };
        let mut node = Some(node);
        let done = self.insert_inner(target, dir, edge.after(), &mut node, next_id);
        if done {
            self.normalize();
        }
        done
    }

    fn insert_inner(
        &mut self,
        target: PaneId,
        dir: Dir,
        after: bool,
        node: &mut Option<Node>,
        next_id: &mut impl FnMut() -> NodeId,
    ) -> bool {
        match self {
            Node::Pane { pane } if *pane == target => {
                let old = std::mem::replace(self, Node::pane(target));
                let new = node.take().expect("inserted once");
                let (a, b) = if after { (old, new) } else { (new, old) };
                *self = Node::Split {
                    id: next_id(),
                    dir,
                    children: vec![Child { weight: 0.5, node: a }, Child { weight: 0.5, node: b }],
                };
                true
            }
            Node::Pane { .. } => false,
            Node::Split { dir: split_dir, children, .. } => {
                // A sibling in a split that already runs the right way.
                if *split_dir == dir
                    && let Some(i) = children.iter().position(|c| c.node == Node::pane(target))
                {
                    let half = children[i].weight / 2.0;
                    children[i].weight = half;
                    let at = if after { i + 1 } else { i };
                    children.insert(at, Child { weight: half, node: node.take().expect("inserted once") });
                    return true;
                }
                children.iter_mut().any(|c| c.node.insert_inner(target, dir, after, node, next_id))
            }
        }
    }

    /// Replace pane `old` with `new` in place. Returns false if `old` is
    /// not in the tree.
    pub fn replace_pane(&mut self, old: PaneId, new: PaneId) -> bool {
        match self {
            Node::Pane { pane } if *pane == old => {
                *pane = new;
                true
            }
            Node::Pane { .. } => false,
            Node::Split { children, .. } => children.iter_mut().any(|c| c.node.replace_pane(old, new)),
        }
    }

    /// The fewest cells the tree fits in: one per pane, plus the one-cell
    /// dividers between neighbours. A tab is never smaller (as in tmux).
    pub fn min_size(&self) -> (u16, u16) {
        match self {
            Node::Pane { .. } => (1, 1),
            Node::Split { dir, children, .. } => {
                let mins = children.iter().map(|c| c.node.min_size());
                let gaps = children.len().saturating_sub(1) as u16;
                match dir {
                    Dir::Row => mins.fold((gaps, 1), |(c, r), (mc, mr)| (c.saturating_add(mc), r.max(mr))),
                    Dir::Column => mins.fold((1, gaps), |(c, r), (mc, mr)| (c.max(mc), r.saturating_add(mr))),
                }
            }
        }
    }

    pub fn has_split(&self, split: NodeId) -> bool {
        match self {
            Node::Pane { .. } => false,
            Node::Split { id, children, .. } => *id == split || children.iter().any(|c| c.node.has_split(split)),
        }
    }

    pub fn find_split_mut(&mut self, split: NodeId) -> Option<(&mut Dir, &mut Vec<Child>)> {
        match self {
            Node::Pane { .. } => None,
            Node::Split { id, dir, children } => {
                if *id == split {
                    return Some((dir, children));
                }
                children.iter_mut().find_map(|c| c.node.find_split_mut(split))
            }
        }
    }

    /// Restore the invariants after an edit.
    pub fn normalize(&mut self) {
        let Node::Split { dir, children, .. } = self else {
            return;
        };
        let dir = *dir;
        for c in children.iter_mut() {
            c.node.normalize();
        }
        // Pull same-direction child splits up into this one.
        let mut flat = Vec::with_capacity(children.len());
        for c in std::mem::take(children) {
            match c.node {
                Node::Split { dir: d, children: grand, .. } if d == dir => {
                    flat.extend(grand.into_iter().map(|g| Child { weight: g.weight * c.weight, node: g.node }));
                }
                node => flat.push(Child { weight: c.weight, node }),
            }
        }
        *children = flat;
        normalize_weights(children);
        if children.len() == 1 {
            let only = children.pop().unwrap().node;
            *self = only;
        }
    }

    /// Check the invariants; for tests and debug assertions.
    pub fn validate(&self) -> Result<(), String> {
        let panes = self.panes();
        let mut seen = std::collections::HashSet::new();
        if let Some(dup) = panes.iter().find(|p| !seen.insert(**p)) {
            return Err(format!("pane {dup} appears twice"));
        }
        self.validate_inner(None)
    }

    fn validate_inner(&self, parent: Option<Dir>) -> Result<(), String> {
        let Node::Split { id, dir, children } = self else {
            return Ok(());
        };
        if children.len() < 2 {
            return Err(format!("split {id} has {} children", children.len()));
        }
        if parent == Some(*dir) {
            return Err(format!("split {id} runs the same way as its parent"));
        }
        let sum: f64 = children.iter().map(|c| c.weight).sum();
        if children.iter().any(|c| !c.weight.is_finite() || c.weight <= 0.0) || (sum - 1.0).abs() > 1e-6 {
            return Err(format!(
                "split {id} has bad weights {:?}",
                children.iter().map(|c| c.weight).collect::<Vec<_>>()
            ));
        }
        children.iter().try_for_each(|c| c.node.validate_inner(Some(*dir)))
    }
}

/// Make weights positive and sum to 1 (equal shares if they can't be).
pub fn normalize_weights(children: &mut [Child]) {
    for c in children.iter_mut() {
        if !c.weight.is_finite() || c.weight <= 0.0 {
            c.weight = 0.0;
        }
    }
    let sum: f64 = children.iter().map(|c| c.weight).sum();
    let n = children.len() as f64;
    for c in children.iter_mut() {
        c.weight = if sum > 0.0 { c.weight / sum } else { 1.0 / n };
        // Keep every child visible.
        c.weight = c.weight.max(0.02 / n);
    }
    let sum: f64 = children.iter().map(|c| c.weight).sum();
    for c in children.iter_mut() {
        c.weight /= sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> impl FnMut() -> NodeId {
        let mut n = 100;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn split_a_pane_then_its_sibling_in_the_same_direction() {
        let mut next = ids();
        let mut t = Node::pane(1);
        assert!(t.insert(1, Edge::Right, Node::pane(2), &mut next));
        assert!(t.insert(2, Edge::Right, Node::pane(3), &mut next));
        t.validate().unwrap();
        // One row of three, not nested splits.
        let Node::Split { dir: Dir::Row, children, .. } = &t else { panic!("{t:?}") };
        assert_eq!(children.len(), 3);
        assert_eq!(t.panes(), vec![1, 2, 3]);
        let w: Vec<f64> = children.iter().map(|c| c.weight).collect();
        assert_eq!(w, vec![0.5, 0.25, 0.25]);
    }

    #[test]
    fn split_across_directions_nests() {
        let mut next = ids();
        let mut t = Node::pane(1);
        t.insert(1, Edge::Right, Node::pane(2), &mut next);
        t.insert(2, Edge::Top, Node::pane(3), &mut next);
        t.validate().unwrap();
        assert_eq!(t.panes(), vec![1, 3, 2]);
    }

    #[test]
    fn removing_collapses_and_reweights() {
        let mut next = ids();
        let mut t = Node::pane(1);
        t.insert(1, Edge::Right, Node::pane(2), &mut next);
        t.insert(2, Edge::Bottom, Node::pane(3), &mut next);
        let t = t.remove(2).unwrap();
        t.validate().unwrap();
        assert_eq!(t.panes(), vec![1, 3]);
        let t = t.remove(3).unwrap();
        assert_eq!(t, Node::pane(1));
        assert_eq!(t.remove(1), None);
    }

    #[test]
    fn inserting_a_subtree_flattens_same_direction() {
        let mut next = ids();
        let mut a = Node::pane(1);
        let mut b = Node::pane(2);
        b.insert(2, Edge::Right, Node::pane(3), &mut next);
        a.insert(1, Edge::Right, b, &mut next);
        a.validate().unwrap();
        let Node::Split { children, .. } = &a else { panic!() };
        assert_eq!(children.len(), 3);
    }

    #[test]
    fn center_is_not_an_insert() {
        let mut t = Node::pane(1);
        assert!(!t.insert(1, Edge::Center, Node::pane(2), &mut ids()));
        assert!(!t.insert(9, Edge::Left, Node::pane(2), &mut ids()));
    }
}
