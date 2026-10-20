//! 序列 diff：patience 取锚点，锚点之间递归，无锚点时退到带上限的 Myers。

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

const MYERS_MAX_D: usize = 1500;

pub(crate) fn diff(a: &[u32], b: &[u32]) -> Vec<Edit> {
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    range(a, b, 0, a.len(), 0, b.len(), &mut out);
    out
}

fn replace(a0: usize, a1: usize, b0: usize, b1: usize, out: &mut Vec<Edit>) {
    out.extend((a0..a1).map(Edit::Delete));
    out.extend((b0..b1).map(Edit::Insert));
}

fn range(
    a: &[u32],
    b: &[u32],
    mut a0: usize,
    mut a1: usize,
    mut b0: usize,
    mut b1: usize,
    out: &mut Vec<Edit>,
) {
    while a0 < a1 && b0 < b1 && a[a0] == b[b0] {
        out.push(Edit::Equal(a0, b0));
        a0 += 1;
        b0 += 1;
    }
    let mut suffix = 0;
    while a0 < a1 && b0 < b1 && a[a1 - 1] == b[b1 - 1] {
        a1 -= 1;
        b1 -= 1;
        suffix += 1;
    }
    if a0 == a1 || b0 == b1 {
        replace(a0, a1, b0, b1, out);
    } else {
        inner(a, b, a0, a1, b0, b1, out);
    }
    out.extend((0..suffix).map(|s| Edit::Equal(a1 + s, b1 + s)));
}

fn inner(a: &[u32], b: &[u32], a0: usize, a1: usize, b0: usize, b1: usize, out: &mut Vec<Edit>) {
    // (a 中次数, a 中位置, b 中次数, b 中位置)
    let mut seen: HashMap<u32, (u32, usize, u32, usize)> = HashMap::new();
    for (i, &h) in a.iter().enumerate().take(a1).skip(a0) {
        let e = seen.entry(h).or_insert((0, i, 0, 0));
        e.0 += 1;
    }
    let mut common = false;
    for (j, &h) in b.iter().enumerate().take(b1).skip(b0) {
        if let Some(e) = seen.get_mut(&h) {
            e.2 += 1;
            e.3 = j;
            common = true;
        }
    }
    if !common {
        replace(a0, a1, b0, b1, out);
        return;
    }
    let mut anchors: Vec<(usize, usize)> = (a0..a1)
        .filter_map(|i| {
            let e = seen[&a[i]];
            (e.0 == 1 && e.2 == 1).then_some((i, e.3))
        })
        .collect();
    anchors = lis(&anchors);
    if anchors.is_empty() {
        match myers(&a[a0..a1], &b[b0..b1], MYERS_MAX_D) {
            Some(ops) => {
                let (mut x, mut y) = (a0, b0);
                for op in ops {
                    match op {
                        Op::Equal => {
                            out.push(Edit::Equal(x, y));
                            x += 1;
                            y += 1;
                        }
                        Op::Delete => {
                            out.push(Edit::Delete(x));
                            x += 1;
                        }
                        Op::Insert => {
                            out.push(Edit::Insert(y));
                            y += 1;
                        }
                    }
                }
            }
            None => replace(a0, a1, b0, b1, out),
        }
        return;
    }
    let (mut pa, mut pb) = (a0, b0);
    for (ai, bi) in anchors {
        range(a, b, pa, ai, pb, bi, out);
        out.push(Edit::Equal(ai, bi));
        pa = ai + 1;
        pb = bi + 1;
    }
    range(a, b, pa, a1, pb, b1, out);
}

/// 按 b 下标求最长严格递增子序列，输入已按 a 下标升序。
fn lis(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; pairs.len()];
    for (k, &(_, bi)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < bi);
        if pos > 0 {
            prev[k] = Some(tails[pos - 1]);
        }
        if pos == tails.len() {
            tails.push(k);
        } else {
            tails[pos] = k;
        }
    }
    let mut res = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(k) = cur {
        res.push(pairs[k]);
        cur = prev[k];
    }
    res.reverse();
    res
}

#[derive(Clone, Copy)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// 经典 Myers；编辑距离超过 `max_d` 时返回 None。
fn myers(a: &[u32], b: &[u32], max_d: usize) -> Option<Vec<Op>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = ((n + m) as usize).min(max_d) as isize;
    let off = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=max {
        trace.push(v[(off - d - 1) as usize..=(off + d + 1) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let idx = (off + k) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, d));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, last_d: isize) -> Vec<Op> {
    let (mut x, mut y) = (n, m);
    let mut ops = Vec::new();
    for d in (0..=last_d).rev() {
        let vs = &trace[d as usize];
        let get = |k: isize| vs[(k + d + 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                ops.push(Op::Insert);
                y -= 1;
            } else {
                ops.push(Op::Delete);
                x -= 1;
            }
        }
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[u32], b: &[u32]) {
        let edits = diff(a, b);
        let (mut ra, mut rb) = (Vec::new(), Vec::new());
        for e in &edits {
            match *e {
                Edit::Equal(i, j) => {
                    assert_eq!(a[i], b[j]);
                    ra.push(a[i]);
                    rb.push(b[j]);
                }
                Edit::Delete(i) => ra.push(a[i]),
                Edit::Insert(j) => rb.push(b[j]),
            }
        }
        assert_eq!(ra, a);
        assert_eq!(rb, b);
    }

    #[test]
    fn reconstructs_inputs() {
        apply(&[], &[]);
        apply(&[1, 2, 3], &[]);
        apply(&[], &[1, 2]);
        apply(&[1, 2, 3, 4], &[2, 1, 4, 3]);
        apply(&[1, 1, 1, 2, 1], &[1, 2, 1, 1, 3]);
        apply(&[1, 2, 3], &[4, 5, 6]);
    }
}
