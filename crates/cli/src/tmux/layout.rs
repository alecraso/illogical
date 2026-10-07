//! An Arugula split tree to and from a tmux layout string (spike S11's
//! `tmux_layout.py`).
//!
//! Cells come from the daemon's own arithmetic (`arugula_core::layout`),
//! so the string is exactly what every client draws: `WxH,X,Y,ID` leaves,
//! `{...}` for a row (side by side), `[...]` for a column, and
//! `layout-custom.c`'s checksum in front. Parsing one back gives cell
//! extents, and weights of extent ÷ total reproduce those cells exactly.

#[cfg(test)]
use arugula_core::Child;
use arugula_core::layout::{Layout, layout};
#[cfg(test)]
use arugula_proto::NodeId;
use arugula_proto::{Dir, Node, PaneId};

/// `layout_checksum()` from tmux's layout-custom.c.
pub fn checksum(body: &str) -> u16 {
    let mut csum: u16 = 0;
    for b in body.bytes() {
        csum = (csum >> 1) + ((csum & 1) << 15);
        csum = csum.wrapping_add(b as u16);
    }
    csum
}

fn with_checksum(body: String) -> String {
    format!("{:04x},{body}", checksum(&body))
}

/// The layout string for `root` in a `cols`×`rows` window.
pub fn to_tmux(root: &Node, cols: u16, rows: u16) -> String {
    let l = layout(root, cols, rows);
    let mut body = String::new();
    write_body(root, &l, &mut body);
    with_checksum(body)
}

/// One pane filling the window: what a zoomed window shows.
pub fn single(pane: PaneId, cols: u16, rows: u16) -> String {
    with_checksum(format!("{cols}x{rows},0,0,{pane}"))
}

const NO_RECT: arugula_proto::Rect = arugula_proto::Rect { x: 0, y: 0, cols: 0, rows: 0 };

fn write_body(node: &Node, l: &Layout, out: &mut String) {
    use std::fmt::Write;
    match node {
        Node::Pane { pane } => {
            let r = l.panes.iter().find(|(p, _)| p == pane).map(|(_, r)| *r).unwrap_or(NO_RECT);
            let _ = write!(out, "{}x{},{},{},{pane}", r.cols, r.rows, r.x, r.y);
        }
        Node::Split { id, dir, children } => {
            let r = l.splits.iter().find(|s| s.id == *id).map(|s| s.rect).unwrap_or(NO_RECT);
            let _ = write!(out, "{}x{},{},{}", r.cols, r.rows, r.x, r.y);
            let (open, close) = if *dir == Dir::Row { ('{', '}') } else { ('[', ']') };
            out.push(open);
            for (i, c) in children.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_body(&c.node, l, out);
            }
            out.push(close);
        }
    }
}

/// A parsed layout string: cells with their sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    Pane { pane: PaneId, cols: u16, rows: u16 },
    Split { dir: Dir, cols: u16, rows: u16, children: Vec<Cell> },
}

impl Cell {
    pub fn size(&self) -> (u16, u16) {
        match self {
            Cell::Pane { cols, rows, .. } | Cell::Split { cols, rows, .. } => (*cols, *rows),
        }
    }

    /// How long the cell is along `dir`.
    #[cfg(test)]
    fn extent(&self, dir: Dir) -> u16 {
        let (c, r) = self.size();
        if dir == Dir::Row { c } else { r }
    }

    /// A tree with weights from the cells. Split ids come from `next_id`.
    #[cfg(test)]
    pub fn to_node(&self, next_id: &mut impl FnMut() -> NodeId) -> Node {
        match self {
            Cell::Pane { pane, .. } => Node::pane(*pane),
            Cell::Split { dir, children, .. } => {
                let total: f64 = children.iter().map(|c| c.extent(*dir) as f64).sum();
                let id = next_id();
                Node::Split {
                    id,
                    dir: *dir,
                    children: children
                        .iter()
                        .map(|c| Child { weight: c.extent(*dir) as f64 / total, node: c.to_node(next_id) })
                        .collect(),
                }
            }
        }
    }
}

/// Parse a layout string, checking its checksum.
pub fn parse(s: &str) -> Result<Cell, String> {
    let (sum, body) = s.split_once(',').ok_or("bad layout")?;
    let sum = u16::from_str_radix(sum, 16).map_err(|_| "bad layout checksum")?;
    if sum != checksum(body) {
        return Err("invalid layout: checksum".into());
    }
    let mut p = Parser { s: body.as_bytes(), i: 0 };
    let cell = p.cell()?;
    if p.i != p.s.len() {
        return Err("invalid layout: trailing characters".into());
    }
    Ok(cell)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn number(&mut self) -> Result<u32, String> {
        let start = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().map_err(|_| "invalid layout: number".to_owned())
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("invalid layout: expected {}", c as char))
        }
    }

    fn cell(&mut self) -> Result<Cell, String> {
        let cols = self.number()? as u16;
        self.expect(b'x')?;
        let rows = self.number()? as u16;
        self.expect(b',')?;
        self.number()?;
        self.expect(b',')?;
        self.number()?;
        match self.s.get(self.i) {
            Some(b',') => {
                self.i += 1;
                let pane = self.number()?;
                Ok(Cell::Pane { pane, cols, rows })
            }
            Some(&open @ (b'{' | b'[')) => {
                self.i += 1;
                let (dir, close) = if open == b'{' { (Dir::Row, b'}') } else { (Dir::Column, b']') };
                let mut children = vec![self.cell()?];
                while self.s.get(self.i) == Some(&b',') {
                    self.i += 1;
                    children.push(self.cell()?);
                }
                self.expect(close)?;
                Ok(Cell::Split { dir, cols, rows, children })
            }
            _ => Err("invalid layout".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use arugula_proto::Node;
    use proptest::prelude::*;

    use super::*;

    fn pane(p: PaneId) -> Node {
        Node::pane(p)
    }

    fn split(dir: Dir, kids: Vec<(f64, Node)>) -> Node {
        Node::Split { id: 0, dir, children: kids.into_iter().map(|(weight, node)| Child { weight, node }).collect() }
    }

    /// Number splits and panes in reading order (tmux numbers its panes in
    /// the order it creates cells).
    fn label(n: &Node, panes: &mut u32, splits: &mut u32) -> Node {
        match n {
            Node::Pane { .. } => {
                *panes += 1;
                pane(*panes - 1)
            }
            Node::Split { dir, children, .. } => {
                *splits += 1;
                let id = *splits;
                Node::Split {
                    id,
                    dir: *dir,
                    children: children
                        .iter()
                        .map(|c| Child { weight: c.weight, node: label(&c.node, panes, splits) })
                        .collect(),
                }
            }
        }
    }

    fn labelled(n: &Node) -> Node {
        label(n, &mut 0, &mut 0)
    }

    fn ids() -> impl FnMut() -> NodeId {
        let mut n = 0;
        move || {
            n += 1;
            n
        }
    }

    #[test]
    fn the_spikes_examples() {
        let examples = [
            (pane(0), 120, 40, "aafd,120x40,0,0,0"),
            (
                split(Dir::Row, vec![(0.5, pane(0)), (0.5, pane(1))]),
                120,
                40,
                "f91d,120x40,0,0{60x40,0,0,0,59x40,61,0,1}",
            ),
            (
                split(Dir::Row, vec![(65.0 / 119.0, pane(0)), (54.0 / 119.0, pane(1))]),
                120,
                40,
                "2a7e,120x40,0,0{65x40,0,0,0,54x40,66,0,1}",
            ),
            (
                split(Dir::Row, vec![(0.5, pane(1)), (0.5, split(Dir::Column, vec![(0.5, pane(2)), (0.5, pane(3))]))]),
                81,
                25,
                "1280,81x25,0,0{40x25,0,0,1,40x25,41,0[40x12,41,0,2,40x12,41,13,3]}",
            ),
        ];
        for (tree, w, h, want) in examples {
            let mut t = tree.clone();
            if let Node::Split { id, .. } = &mut t {
                *id = 1;
            }
            if let Node::Split { children, .. } = &mut t
                && let Some(Node::Split { id, .. }) = children.get_mut(1).map(|c| &mut c.node)
            {
                *id = 2;
            }
            assert_eq!(to_tmux(&t, w, h), want);
        }
        // The transcript's strings parse and re-derive byte for byte.
        for s in [
            "aafd,120x40,0,0,0",
            "f91d,120x40,0,0{60x40,0,0,0,59x40,61,0,1}",
            "2a7e,120x40,0,0{65x40,0,0,0,54x40,66,0,1}",
            "9ceb,100x30,0,0{55x30,0,0,0,44x30,56,0,1}",
        ] {
            let c = parse(s).unwrap();
            let (w, h) = c.size();
            assert_eq!(to_tmux(&c.to_node(&mut ids()), w, h), s);
        }
        assert!(parse("abcd,120x40,0,0,0").is_err(), "bad checksum");
        assert_eq!(single(4, 80, 24), to_tmux(&pane(4), 80, 24));
    }

    /// A tree like the spike's: 2 or 3 children per split, directions
    /// alternating, up to three levels.
    fn arb_tree() -> impl Strategy<Value = Node> {
        let leaf = Just(pane(0)).boxed();
        leaf.prop_recursive(3, 27, 3, |inner| {
            (any::<bool>(), prop::collection::vec((0.1f64..1.0, inner), 2..=3)).prop_map(|(row, kids)| {
                let dir = if row { Dir::Row } else { Dir::Column };
                let sum: f64 = kids.iter().map(|(w, _)| w).sum();
                let mut n = split(dir, kids.into_iter().map(|(w, n)| (w / sum, n)).collect());
                n.normalize();
                n
            })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(3000))]

        /// derive -> parse -> derive is identical: cells -> weights -> cells
        /// is exact, so a tmux-side change (a divider drag) turns into
        /// weights that reproduce exactly the cells tmux reported.
        #[test]
        fn round_trips(tree in arb_tree(), w in 20u16..400, h in 10u16..150) {
            let t = labelled(&tree);
            let (mw, mh) = t.min_size();
            let (w, h) = (w.max(mw), h.max(mh));
            let s = to_tmux(&t, w, h);
            let c = parse(&s).unwrap();
            prop_assert_eq!(c.size(), (w, h));
            prop_assert_eq!(to_tmux(&c.to_node(&mut ids()), w, h), s);
        }
    }

    /// Real tmux accepts our strings with `select-layout` and reports them
    /// back from `#{window_layout}` byte for byte (S11's `--tmux` check).
    #[test]
    fn real_tmux_agrees() {
        if std::process::Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("tmux not installed; skipping");
            return;
        }
        let sock = format!("arugula-m5-layout-{}", std::process::id());
        let tmux = |args: &[&str]| {
            std::process::Command::new("tmux").args(["-L", &sock, "-f", "/dev/null"]).args(args).output().unwrap()
        };
        use proptest::strategy::ValueTree;
        let mut runner = proptest::test_runner::TestRunner::deterministic();
        let mut cases = vec![];
        for _ in 0..30 {
            let t = labelled(&arb_tree().new_tree(&mut runner).unwrap().current());
            let (mw, mh) = t.min_size();
            cases.push((t, 40u16.max(mw) + cases.len() as u16 * 7, 15u16.max(mh) + cases.len() as u16 * 2));
        }
        let mut ok = 0;
        for (t, w, h) in &cases {
            tmux(&["kill-server"]);
            let (ws, hs) = (w.to_string(), h.to_string());
            // A server that is just going away can refuse the first try.
            let started =
                (0..5).any(|_| tmux(&["new", "-d", "-s", "lay", "-x", &ws, "-y", &hs, "cat"]).status.success());
            assert!(started, "tmux new");
            tmux(&["set", "-g", "window-size", "manual"]);
            tmux(&["resize-window", "-t", "lay:0", "-x", &ws, "-y", &hs]);
            for k in 0..t.panes().len() - 1 {
                tmux(&["select-layout", "-t", "lay:0", "tiled"]);
                tmux(&["split-window", "-d", "-t", &format!("lay:0.{k}"), "cat"]);
            }
            let want = to_tmux(t, *w, *h);
            let r = tmux(&["select-layout", "-t", "lay:0", &want]);
            let got = String::from_utf8_lossy(&tmux(&["display", "-p", "-t", "lay:0", "#{window_layout}"]).stdout)
                .trim()
                .to_owned();
            assert!(r.status.success(), "tmux refused {want}: {}", String::from_utf8_lossy(&r.stderr));
            assert_eq!(got, want, "{w}x{h}");
            ok += 1;
        }
        tmux(&["kill-server"]);
        assert_eq!(ok, cases.len());
    }
}
