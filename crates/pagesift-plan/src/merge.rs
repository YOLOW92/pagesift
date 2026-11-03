//! 两个候选按块合并。
//!
//! 1. 相同指纹的块用 patience/Myers 对齐（`pagesift_diff::align`），对上的块两边都判为正文，必然保留；
//! 2. 对齐锚点之间的空隙里，按相似度做单调的最大权匹配。"是同一块"的门槛是零假设分布的 `q(1−δ)`：
//!    零假设样本取已知不同的块对（两个锚点块与对方另一锚点块的配对），即同一页上"随机不同块"的相似度；
//! 3. 同一块文字不同（冲突）：取质量后验均值高的一方；均值相同按文字定序，保证与参数顺序无关；
//! 4. 只有一方有的块：模板概率 < ½（更可能是正文）且所在候选的质量区间不劣于对方（上界 ≥ 对方下界）才补入。
//!
//! 第 6 步"合并后的质量不显著低于最好的单个候选"要对合并结果重新打分，由调用方做。

use std::collections::{HashMap, HashSet};

use pagesift_stat::Delta;

use crate::block::{Block, jaccard};

/// 候选的质量：校准后的 P(完整可用) 及其区间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quality {
    pub mean: f64,
    pub lo: f64,
    pub hi: f64,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub blocks: Vec<Block>,
    pub quality: Quality,
}

/// 合并结果里每块的来源（写进 `PlanTrace`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    Both,
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct Merged {
    pub blocks: Vec<Block>,
    pub from: Vec<From>,
}

impl Merged {
    pub fn markdown(&self) -> String {
        crate::block::join(&self.blocks)
    }
}

pub fn merge(left: &Candidate, right: &Candidate, delta: Delta) -> Merged {
    let mut ids = HashMap::new();
    let mut id = |b: &Block| {
        let n = ids.len() as u32;
        *ids.entry(b.fingerprint).or_insert(n)
    };
    let a: Vec<u32> = left.blocks.iter().map(&mut id).collect();
    let b: Vec<u32> = right.blocks.iter().map(&mut id).collect();
    let anchors = pagesift_diff::align(&a, &b);
    let wa: Vec<HashSet<String>> = left.blocks.iter().map(Block::words).collect();
    let wb: Vec<HashSet<String>> = right.blocks.iter().map(Block::words).collect();
    let tau = same_block_threshold(&anchors, &wa, &wb, delta);

    let mut out = Merged {
        blocks: vec![],
        from: vec![],
    };
    let (mut i0, mut j0) = (0, 0);
    for &(i, j) in anchors.iter().chain(std::iter::once(&(a.len(), b.len()))) {
        gap(left, right, (i0, i), (j0, j), &wa, &wb, tau, &mut out);
        if i < a.len() {
            out.blocks.push(left.blocks[i].clone());
            out.from.push(From::Both);
        }
        (i0, j0) = (i + 1, j + 1);
    }
    out
}

/// "同一块"的相似度门槛：已知不同块对的相似度的经验 `q(1−δ)`。
/// 样本数取到 `⌈1/δ⌉`（经验分位点上方期望至少有一个样本），锚点不够时用尽所有配对。
/// 没有零假设样本（锚点少于两个）时门槛为 1：只认文字完全一样的块。
fn same_block_threshold(
    anchors: &[(usize, usize)],
    wa: &[HashSet<String>],
    wb: &[HashSet<String>],
    delta: Delta,
) -> f64 {
    let t = anchors.len();
    if t < 2 {
        return 1.0;
    }
    let want = (1.0 / delta.get()).ceil() as usize;
    let shifts = want.div_ceil(t).min(t - 1);
    let mut null: Vec<f64> = (1..=shifts)
        .flat_map(|s| (0..t).map(move |k| (anchors[k].0, anchors[(k + s) % t].1)))
        .map(|(i, j)| jaccard(&wa[i], &wb[j]))
        .collect();
    null.sort_by(f64::total_cmp);
    let k = ((1.0 - delta.get()) * null.len() as f64).ceil() as usize;
    null[k.clamp(1, null.len()) - 1]
}

#[allow(clippy::too_many_arguments)] // 一个空隙的两侧范围和各自的词集合，只在 merge 里调用
fn gap(
    left: &Candidate,
    right: &Candidate,
    (i0, i1): (usize, usize),
    (j0, j1): (usize, usize),
    wa: &[HashSet<String>],
    wb: &[HashSet<String>],
    tau: f64,
    out: &mut Merged,
) {
    let pairs = monotone_matching(i0..i1, j0..j1, |i, j| {
        let s = jaccard(&wa[i], &wb[j]);
        (s > tau).then_some(s)
    });
    let keep_left = |i: usize| keeps(&left.blocks[i], left.quality, right.quality);
    let keep_right = |j: usize| keeps(&right.blocks[j], right.quality, left.quality);
    let (mut i, mut j) = (i0, j0);
    for &(pi, pj) in pairs.iter().chain(std::iter::once(&(i1, j1))) {
        for k in i..pi {
            if keep_left(k) {
                out.blocks.push(left.blocks[k].clone());
                out.from.push(From::Left);
            }
        }
        for k in j..pj {
            if keep_right(k) {
                out.blocks.push(right.blocks[k].clone());
                out.from.push(From::Right);
            }
        }
        if pi < i1 {
            let (l, r) = (&left.blocks[pi], &right.blocks[pj]);
            let left_wins =
                (left.quality.mean, r.text.as_str()) > (right.quality.mean, l.text.as_str());
            out.blocks
                .push(if left_wins { l.clone() } else { r.clone() });
            out.from
                .push(if left_wins { From::Left } else { From::Right });
        }
        (i, j) = (pi + 1, pj + 1);
    }
}

fn keeps(block: &Block, own: Quality, other: Quality) -> bool {
    block.template_prob.is_none_or(|p| p < 0.5) && own.hi >= other.lo
}

/// 两段范围之间的单调匹配（不交叉），使匹配上的相似度之和最大（加权最长公共子序列的动态规划）。
fn monotone_matching(
    is: std::ops::Range<usize>,
    js: std::ops::Range<usize>,
    sim: impl Fn(usize, usize) -> Option<f64>,
) -> Vec<(usize, usize)> {
    let (n, m) = (is.len(), js.len());
    if n == 0 || m == 0 {
        return vec![];
    }
    let s: Vec<Vec<Option<f64>>> = is
        .clone()
        .map(|i| js.clone().map(|j| sim(i, j)).collect())
        .collect();
    let mut best = vec![vec![0.0f64; m + 1]; n + 1];
    for x in (0..n).rev() {
        for y in (0..m).rev() {
            let take = s[x][y].map_or(f64::NEG_INFINITY, |v| v + best[x + 1][y + 1]);
            best[x][y] = take.max(best[x + 1][y]).max(best[x][y + 1]);
        }
    }
    let (mut x, mut y, mut out) = (0, 0, vec![]);
    while x < n && y < m {
        if let Some(v) = s[x][y]
            && best[x][y] == v + best[x + 1][y + 1]
        {
            out.push((is.start + x, js.start + y));
            (x, y) = (x + 1, y + 1);
        } else if best[x][y] == best[x + 1][y] {
            x += 1;
        } else {
            y += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::split;

    fn cand(md: &str, mean: f64) -> Candidate {
        Candidate {
            blocks: split(md),
            quality: Quality {
                mean,
                lo: mean - 0.1,
                hi: mean + 0.1,
            },
        }
    }

    fn set(m: &Merged) -> Vec<String> {
        let mut v: Vec<String> = m.blocks.iter().map(|b| b.text.clone()).collect();
        v.sort();
        v
    }

    const BASE: &str = "# Title\n\nFirst paragraph about rust.\n\nSecond paragraph about cats.\n\nThird paragraph about dogs.\n\nFourth paragraph about birds.\n";

    #[test]
    fn identical_candidates_merge_to_the_input() {
        let m = merge(&cand(BASE, 0.8), &cand(BASE, 0.8), Delta::default());
        assert_eq!(m.markdown(), BASE);
        assert!(m.from.iter().all(|f| *f == From::Both));
    }

    #[test]
    fn keeps_every_shared_block_and_fills_in_missing_ones() {
        let http = cand(
            "# Title\n\nFirst paragraph about rust.\n\nFourth paragraph about birds.\n",
            0.7,
        );
        let browser = cand(BASE, 0.7);
        let m = merge(&http, &browser, Delta::default());
        assert_eq!(m.markdown(), BASE);
        let shared = [
            "# Title",
            "First paragraph about rust.",
            "Fourth paragraph about birds.",
        ];
        assert!(shared.iter().all(|s| m.blocks.iter().any(|b| b.text == *s)));
    }

    #[test]
    fn result_set_does_not_depend_on_argument_order() {
        let x = cand(
            "# Title\n\nFirst paragraph about rust.\n\nSecond paragraph about cats, edited.\n\nFourth paragraph about birds.\n\nOnly here.\n",
            0.6,
        );
        let y = cand(BASE, 0.9);
        assert_eq!(
            set(&merge(&x, &y, Delta::default())),
            set(&merge(&y, &x, Delta::default()))
        );
    }

    #[test]
    fn conflicting_block_comes_from_the_better_candidate_and_template_blocks_are_dropped() {
        let mut x = cand(
            "# Title\n\nFirst paragraph about rust.\n\nSecond paragraph about cats and more.\n\nThird paragraph about dogs.\n\nFourth paragraph about birds.\n",
            0.95,
        );
        let y = cand(BASE, 0.5);
        let m = merge(&x, &y, Delta::default());
        assert!(
            m.blocks
                .iter()
                .any(|b| b.text == "Second paragraph about cats and more.")
        );
        assert!(
            !m.blocks
                .iter()
                .any(|b| b.text == "Second paragraph about cats.")
        );
        x.blocks.push(crate::block::Block {
            template_prob: Some(0.9),
            ..crate::block::Block::new(crate::block::Kind::Paragraph, "Subscribe now".into())
        });
        let m = merge(&x, &y, Delta::default());
        assert!(!m.blocks.iter().any(|b| b.text == "Subscribe now"));
    }
}
