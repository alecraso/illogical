//! A proposed edit as a unified diff, for the diff card (M28).

/// Lines of context around each change.
const CONTEXT: usize = 3;
/// Past this many lines on either side of the changed middle, the middle is
/// shown as one block instead of being matched line by line.
const MAX_MATCH: usize = 4000;

/// Lines added, lines removed, and the unified diff (hunks only, no file
/// headers), cut at `cap` bytes.
pub fn unified(old: &str, new: &str, cap: usize) -> (u32, u32, String) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = ops(&a, &b);
    let added = ops.iter().filter(|o| matches!(o, Op::Add(_))).count() as u32;
    let removed = ops.iter().filter(|o| matches!(o, Op::Del(_))).count() as u32;
    // Where each side stands before each op.
    let mut pos = Vec::with_capacity(ops.len());
    let (mut i, mut j) = (0, 0);
    for op in &ops {
        pos.push((i, j));
        match op {
            Op::Same(..) => (i, j) = (i + 1, j + 1),
            Op::Del(_) => i += 1,
            Op::Add(_) => j += 1,
        }
    }
    let mut out = String::new();
    for (s, e) in hunks(&ops) {
        let (ai, bi) = pos[s];
        let an = ops[s..e].iter().filter(|o| !matches!(o, Op::Add(_))).count();
        let bn = ops[s..e].iter().filter(|o| !matches!(o, Op::Del(_))).count();
        // An empty side names the line before it.
        let at = |start: usize, n: usize| if n == 0 { start } else { start + 1 };
        out.push_str(&format!("@@ -{},{an} +{},{bn} @@\n", at(ai, an), at(bi, bn)));
        for op in &ops[s..e] {
            let (mark, line) = match op {
                Op::Same(i, _) => (' ', a[*i]),
                Op::Del(i) => ('-', a[*i]),
                Op::Add(j) => ('+', b[*j]),
            };
            out.push(mark);
            out.push_str(line);
            out.push('\n');
        }
        if out.len() > cap {
            let mut end = cap;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push_str("\n…\n");
            break;
        }
    }
    (added, removed, out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Line `i` of the old, which is line `j` of the new.
    Same(usize, usize),
    Del(usize),
    Add(usize),
}

/// The edit script: common lines at both ends, the middle matched by
/// longest common subsequence when it's small enough.
fn ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let mut out: Vec<Op> = (0..pre).map(|i| Op::Same(i, i)).collect();
    if am.len() <= MAX_MATCH && bm.len() <= MAX_MATCH && am.len() * bm.len() <= 4_000_000 {
        // lcs[i][j]: the longest common subsequence of am[i..] and bm[j..].
        let (n, m) = (am.len(), bm.len());
        let mut lcs = vec![0u16; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * (m + 1) + j] = if am[i] == bm[j] {
                    lcs[(i + 1) * (m + 1) + j + 1].saturating_add(1)
                } else {
                    lcs[(i + 1) * (m + 1) + j].max(lcs[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                out.push(Op::Same(pre + i, pre + j));
                (i, j) = (i + 1, j + 1);
            } else if i < n && (j == m || lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1]) {
                // Removals before additions, as diff shows them.
                out.push(Op::Del(pre + i));
                i += 1;
            } else {
                out.push(Op::Add(pre + j));
                j += 1;
            }
        }
    } else {
        out.extend((0..am.len()).map(|i| Op::Del(pre + i)));
        out.extend((0..bm.len()).map(|j| Op::Add(pre + j)));
    }
    out.extend((0..suf).map(|k| Op::Same(a.len() - suf + k, b.len() - suf + k)));
    out
}

/// Ranges of ops to show: each change with its context, close ones merged.
fn hunks(ops: &[Op]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = vec![];
    for (k, op) in ops.iter().enumerate() {
        if matches!(op, Op::Same(..)) {
            continue;
        }
        let (s, e) = (k.saturating_sub(CONTEXT), (k + 1 + CONTEXT).min(ops.len()));
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_changed_in_the_middle() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\n";
        let (add, del, text) = unified(old, new, 10_000);
        assert_eq!((add, del), (1, 1));
        assert_eq!(text, "@@ -2,7 +2,7 @@\n b\n c\n d\n-e\n+E\n f\n g\n h\n");
    }

    #[test]
    fn a_new_file_is_all_added() {
        let (add, del, text) = unified("", "x\ny\n", 10_000);
        assert_eq!((add, del), (2, 0));
        assert_eq!(text, "@@ -0,0 +1,2 @@\n+x\n+y\n");
    }

    #[test]
    fn far_apart_changes_are_two_hunks() {
        let old: String = (0..30).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "two\n").replace("l25\n", "");
        let (add, del, text) = unified(&old, &new, 10_000);
        assert_eq!((add, del), (1, 2));
        assert_eq!(text.matches("@@").count(), 4, "{text}");
        assert!(text.contains("-l2\n+two\n"));
        assert!(text.contains("@@ -23,7 +23,6 @@\n l22\n l23\n l24\n-l25\n l26\n"), "{text}");
    }

    #[test]
    fn long_diffs_are_cut() {
        let new: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let (add, _, text) = unified("", &new, 200);
        assert_eq!(add, 1000);
        assert!(text.len() < 220 && text.ends_with("…\n"));
    }
}
