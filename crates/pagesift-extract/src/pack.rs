//! 把一串相邻片段（块、行、句子……）合并成块：决定块边界落在哪些片段之间（自研，0.88 W9）。
//!
//! 两种方式：
//! - [`Pack::Greedy`]：0.81 的做法（semchunk）。从左到右往一块里塞，塞到下一片放不下为止；块长贴着
//!   预算，最后常剩一小截。queryFilter 的打分单位仍用它（它的参数是按这个行为评测出来的）。
//! - [`Pack::Structure`]：按块结构合并。
//!   1. 不超过预算的单个片段（一整段、一整张表、一整个列表）永远整块保留，不切开；
//!   2. 片段是整块（Markdown 块、段落、行）时，在它们之间切不会切断内容，合并得小一些：块长不超过
//!      `max(预算/2, 这一串里最长的块)`。预算的一半保证一个预算里至少放得下两块合并出来的块（检索
//!      第一名不对时第二名还能进上下文）；最长块说明这一串的自然块就有这么长，小块合到它那么长不算稀释；
//!   3. 片段是一个超长块内部切出来的（句子、子句、词）时，每多切一刀都可能切断一句话，所以切得最少：
//!      上限就是预算；
//!   4. 在上限下用最少的块数，再在同样块数里取最小的块长上限（线性划分，min-max），各块长度均衡，
//!      不留零头。
//!
//!   块长由预算和这一串片段的长度分布共同决定，没有另外的块长或重叠参数。
//!   （0.88 的评测里，块内部也按预算一半合并时，块预算 100 的召回和 IoU 下降，见 docs/history/0.88_rag.md。）

use std::ops::Range;

/// 合并方式，见模块说明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pack {
    Greedy,
    Structure,
}

/// 一串片段的上下文：片段之间分隔符的 token 数、哪些片段是标题、这串后面是否还有内容。
pub(crate) struct Run<'a> {
    /// 每个片段的 token 数；调用方保证每个都不超过预算。
    pub tokens: &'a [usize],
    /// 非空白分隔符（中日韩标点）自身的 token 数，接在每个片段后面；空白分隔时为 0。
    pub sep_t: usize,
    /// 第 i 个片段是不是 Markdown 标题（标题不放在一块的结尾）。
    pub heading: &'a dyn Fn(usize) -> bool,
    /// 这串之后文档里还有内容（标题后移的规则只在后面还有东西时生效）。
    pub more_after: bool,
    /// 片段是整块（Markdown 块、段落、行），不是一个超长块内部切出来的句子或词。
    pub blocks: bool,
}

/// 把 `run` 分组，返回每组的片段下标区间（按顺序、首尾相接、覆盖全部片段）。
pub(crate) fn group(run: &Run, size: usize, pack: Pack) -> Vec<Range<usize>> {
    if run.tokens.is_empty() {
        return Vec::new();
    }
    match pack {
        Pack::Greedy => fill(run, size),
        Pack::Structure => {
            let longest = run.tokens.iter().copied().max().unwrap_or(0);
            let cap = if run.blocks {
                // [推导] 预算的一半：一个预算里至少放得下两块合并出来的块（见模块说明第 2 条）
                (size / 2).max(longest).min(size)
            } else {
                size
            };
            let k = fill(run, cap).len();
            // 线性划分：块数不超过 k 的最小上限。fill 的块数随上限单调不增（标题后移只让块数
            // 在少数位置多 1），二分后再核对一次，不满足就退回 cap。
            let (mut lo, mut hi) = (longest.min(cap), cap);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if fill(run, mid).len() <= k {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            let out = fill(run, lo);
            if out.len() <= k { out } else { fill(run, cap) }
        }
    }
}

/// 从左到右贪心：往当前块里加片段，直到再加一个就超过 `cap`；一块不以标题结尾。
fn fill(run: &Run, cap: usize) -> Vec<Range<usize>> {
    let t = run.tokens;
    let n = t.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let mut acc = t[i];
        let mut j = i + 1;
        while j < n && acc + run.sep_t + t[j] + run.sep_t <= cap {
            acc += run.sep_t + t[j];
            j += 1;
        }
        // 一块不以标题结尾（标题后面还有内容时）
        while (j < n || run.more_after) && j - 1 > i && (run.heading)(j - 1) {
            j -= 1;
        }
        out.push(i..j);
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(tokens: &[usize]) -> Run<'_> {
        Run {
            tokens,
            sep_t: 0,
            heading: &|_| false,
            more_after: false,
            blocks: true,
        }
    }

    fn inside(tokens: &[usize]) -> Run<'_> {
        Run {
            blocks: false,
            ..plain(tokens)
        }
    }

    fn sums(tokens: &[usize], groups: &[Range<usize>]) -> Vec<usize> {
        groups
            .iter()
            .map(|g| tokens[g.clone()].iter().sum())
            .collect()
    }

    /// 暴力求"最少块数下的最小块长上限"，核对线性划分。
    fn brute(tokens: &[usize], cap: usize) -> (usize, usize) {
        let n = tokens.len();
        let mut best = (usize::MAX, usize::MAX);
        for mask in 0u32..(1 << (n - 1)) {
            let mut groups = Vec::new();
            let mut acc = tokens[0];
            for (i, &t) in tokens.iter().enumerate().skip(1) {
                if mask & (1 << (i - 1)) != 0 {
                    groups.push(acc);
                    acc = t;
                } else {
                    acc += t;
                }
            }
            groups.push(acc);
            let max = *groups.iter().max().unwrap();
            if max <= cap {
                best = best.min((groups.len(), max));
            }
        }
        best
    }

    #[test]
    fn greedy_fills_up_to_the_budget() {
        let t = [30, 30, 30, 30, 30, 30, 30, 30];
        assert_eq!(
            sums(&t, &group(&plain(&t), 100, Pack::Greedy)),
            [90, 90, 60]
        );
    }

    #[test]
    fn structure_merges_to_half_the_budget_and_balances() {
        let t = [30, 30, 30, 30, 30, 30, 30, 30];
        // 上限 max(200/2, 30) = 100：最少 3 块；3 块时最大块长最小是 90（上限 89 就要 4 块）
        let g = group(&plain(&t), 200, Pack::Structure);
        assert_eq!(sums(&t, &g), [90, 90, 60]);
        // 同样的片段，贪心在预算 200 下塞到 180
        assert_eq!(sums(&t, &group(&plain(&t), 200, Pack::Greedy)), [180, 60]);
        // 6 × 20：上限 100 下贪心是 100 + 20（留一截零头），均衡后是 60 + 60
        let t = [20, 20, 20, 20, 20, 20];
        let g = group(&plain(&t), 200, Pack::Structure);
        assert_eq!(sums(&t, &g), [60, 60]);
        let t = [30, 30, 30, 30, 30, 30, 30];
        // 贪心在同样的预算下会塞到 180
        assert_eq!(sums(&t, &group(&plain(&t), 200, Pack::Greedy)), [180, 30]);
    }

    #[test]
    fn inside_a_block_cuts_as_few_times_as_possible() {
        // 一个 300 词的段落切成 12 句、每句 25 词：预算 200 下最少 2 块，均衡成 150 + 150
        let t = [25; 12];
        let g = group(&inside(&t), 200, Pack::Structure);
        assert_eq!(sums(&t, &g), [150, 150]);
        // 同样的片段如果是 12 个整段，合并上限是预算的一半：3 块、每块 100
        let g = group(&plain(&t), 200, Pack::Structure);
        assert_eq!(sums(&t, &g), [100, 100, 100]);
        // 贪心：200 + 100
        assert_eq!(sums(&t, &group(&inside(&t), 200, Pack::Greedy)), [200, 100]);
    }

    #[test]
    fn structure_keeps_long_pieces_whole_and_uses_them_as_the_scale() {
        // 一个 150 的整段：不切；它决定这一串的上限是 150
        let t = [150, 40, 40, 40, 20];
        let g = group(&plain(&t), 200, Pack::Structure);
        assert!(g.contains(&(0..1)), "{g:?}");
        assert!(sums(&t, &g).iter().all(|&s| s <= 150));
        assert!(sums(&t, &g).iter().all(|&s| s <= 200));
    }

    #[test]
    fn structure_is_the_min_max_partition() {
        let mut seed: u64 = 11;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let n = 1 + (next() % 9) as usize;
            let size = 2 + (next() % 60) as usize;
            let t: Vec<usize> = (0..n)
                .map(|_| (next() % (size as u64 + 1)) as usize)
                .collect();
            let longest = *t.iter().max().unwrap();
            for blocks in [true, false] {
                let run = Run {
                    blocks,
                    ..plain(&t)
                };
                let g = group(&run, size, Pack::Structure);
                let s = sums(&t, &g);
                // 首尾相接、覆盖全部
                assert_eq!(g.first().unwrap().start, 0);
                assert_eq!(g.last().unwrap().end, n);
                assert!(g.windows(2).all(|w| w[0].end == w[1].start));
                let cap = if blocks {
                    (size / 2).max(longest).min(size)
                } else {
                    size
                };
                assert!(s.iter().all(|&x| x <= cap), "{t:?} {size} {s:?}");
                let (k, m) = brute(&t, cap);
                assert_eq!(g.len(), k, "{t:?} {size}");
                assert_eq!(*s.iter().max().unwrap(), m, "{t:?} {size}");
            }
        }
    }

    #[test]
    fn headings_never_end_a_chunk() {
        let t = [2, 30, 2, 30, 2, 30];
        let heading = |i: usize| i.is_multiple_of(2);
        let run = Run {
            tokens: &t,
            sep_t: 0,
            heading: &heading,
            more_after: false,
            blocks: true,
        };
        for pack in [Pack::Greedy, Pack::Structure] {
            for g in group(&run, 40, pack) {
                assert!(g.len() == 1 || !heading(g.end - 1), "{pack:?} {g:?}");
            }
        }
    }
}
