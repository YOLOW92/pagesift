//! 两个候选按块合并。
//!
//! 1. 相同指纹的块用 patience/Myers 对齐（`pagesift_diff::align`），对上的块两边都判为正文，必然保留；
//! 2. 对齐锚点之间的空隙里，按相似度做单调的最大权匹配。"是同一块"的门槛是零假设分布的 `q(1−δ)`：
//!    零假设样本取已知不同的块对（两个锚点块与对方另一锚点块的配对），即同一页上"随机不同块"的相似度；
//! 3. 同一块文字不同（冲突）：取质量后验均值高的一方；均值相同按文字定序，保证与参数顺序无关；
//! 4. 只有一方有的块：模板概率 < ½（更可能是正文）且所在候选的质量区间不劣于对方（上界 ≥ 对方下界）才补入。
//!
//! 第 6 步"合并后的质量不显著低于最好的单个候选"要对合并结果重新打分，由调用方做。
//!
//! 第 2 步的动态规划在一个空隙里要 `n×m` 个单元（时间和内存）。两个候选都来自不可信的页面，
//! 块数可以很大而几乎没有锚点（0.90 里两边各 10 000 个互不相同的块就是一亿个单元、几 GB 内存），
//! 所以全部空隙的单元数合计不超过两个候选正文的总字节数：合并的代价与读入候选同阶（线性）。
//! 超出预算的空隙不再逐块配对，整段取质量均值高的一方（与第 3 步冲突时的取法相同），
//! 不会把两边相似的块各留一份。

use std::collections::{HashMap, HashSet};

use pagesift_stat::Delta;

use crate::block::{Block, jaccard};

/// 候选的质量：校准后的相对质量分（`crate::quality`，只用来比较两个候选）及其区间。
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
    let bytes = |c: &Candidate| c.blocks.iter().map(|b| b.text.len()).sum::<usize>();
    let mut budget = bytes(left) + bytes(right);

    let mut out = Merged {
        blocks: vec![],
        from: vec![],
    };
    let (mut i0, mut j0) = (0, 0);
    for &(i, j) in anchors.iter().chain(std::iter::once(&(a.len(), b.len()))) {
        let g = Gap {
            is: i0..i,
            js: j0..j,
            tau,
        };
        gap(left, right, g, &wa, &wb, &mut budget, &mut out);
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

/// 两个锚点之间的一段空隙：左右两边的块范围和"同一块"的门槛。
struct Gap {
    is: std::ops::Range<usize>,
    js: std::ops::Range<usize>,
    tau: f64,
}

/// 空隙超出剩余预算时整段取哪一边：质量均值高的一方；均值相同按块数、再按文字定序，与参数顺序无关。
fn left_side_wins(left: &Candidate, right: &Candidate, g: &Gap) -> bool {
    fn text<'a>(c: &'a Candidate, r: &std::ops::Range<usize>) -> Vec<&'a str> {
        c.blocks[r.clone()]
            .iter()
            .map(|b| b.text.as_str())
            .collect()
    }
    (left.quality.mean, g.is.len(), text(left, &g.is))
        > (right.quality.mean, g.js.len(), text(right, &g.js))
}

fn gap(
    left: &Candidate,
    right: &Candidate,
    g: Gap,
    wa: &[HashSet<String>],
    wb: &[HashSet<String>],
    budget: &mut usize,
    out: &mut Merged,
) {
    let cells = g.is.len().saturating_mul(g.js.len());
    if cells > *budget {
        let (c, r, from) = if left_side_wins(left, right, &g) {
            (left, g.is.clone(), From::Left)
        } else {
            (right, g.js.clone(), From::Right)
        };
        for k in r {
            if c.blocks[k].template_prob.is_none_or(|p| p < 0.5) {
                out.blocks.push(c.blocks[k].clone());
                out.from.push(from);
            }
        }
        return;
    }
    *budget -= cells;
    let (i0, j0, i1, j1, tau) = (g.is.start, g.js.start, g.is.end, g.js.end, g.tau);
    let pairs = monotone_matching(g.is, g.js, |i, j| {
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

    /// 两边各几千个互不相同、没有锚点的块：0.90 要建 n×m 的矩阵（一万对一万是一亿个单元）。
    /// 现在超出线性预算的空隙整段取质量高的一方，很快返回，也不重复留相似的块。
    #[test]
    fn huge_unanchored_gap_stays_linear_and_keeps_the_better_side() {
        let side = |tag: &str, n: usize| -> String {
            (0..n)
                .map(|i| format!("{tag} block number {i} with words w{i}x\n\n"))
                .collect()
        };
        let n = 5000;
        let (x, y) = (cand(&side("left", n), 0.9), cand(&side("right", n), 0.5));
        let (m, took) = pagesift_testkit::cpu_time(|| merge(&x, &y, Delta::default()));
        assert_eq!(m.blocks.len(), n);
        assert!(m.from.iter().all(|f| *f == From::Left));
        assert_eq!(
            set(&merge(&y, &x, Delta::default())),
            set(&m),
            "argument order"
        );
        // 本线程的 CPU 时间，宽松的上限。0.91 开发机（quick 档）上：二次的做法两次合并共 9.4 s、
        // 峰值 598 MB，现在 0.03 s
        assert!(
            took < pagesift_testkit::budget(std::time::Duration::from_millis(100)),
            "{took:?}"
        );
    }

    /// 预算内的小空隙照旧逐块配对。
    #[test]
    fn small_gaps_still_match_block_by_block() {
        let x = cand(
            "# Title\n\nFirst paragraph about rust.\n\nSecond paragraph about cats.\n\nFourth paragraph about birds.\n",
            0.7,
        );
        let y = cand(
            "# Title\n\nFirst paragraph about rust.\n\nSecond paragraph about cats!\n\nThird paragraph about dogs.\n\nFourth paragraph about birds.\n",
            0.7,
        );
        let m = merge(&x, &y, Delta::default());
        assert!(
            m.blocks
                .iter()
                .any(|b| b.text == "Third paragraph about dogs.")
        );
        assert_eq!(
            m.blocks
                .iter()
                .filter(|b| b.text.starts_with("Second"))
                .count(),
            1
        );
    }
}
