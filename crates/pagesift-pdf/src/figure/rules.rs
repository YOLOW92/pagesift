//! 横竖线的查询：短线是不是长线断开的一截（表格框线在交点处常断开），一个外框有几条边贴着框线（表格格子）。

use std::cell::OnceCell;

use crate::geom::Rule;

/// 按位置排好的横线、竖线（用到时才排）。
pub(super) struct Rules<'a> {
    all: &'a [Rule],
    /// 竖线按 x 排、横线按 y 排。
    sorted: OnceCell<(Vec<&'a Rule>, Vec<&'a Rule>)>,
    /// 端点索引（[竖线, 横线]）。判断“断开的一截”用。
    index: OnceCell<[Joints<'a>; 2]>,
}

/// 一个方向的端点索引：每条线（所在位置×2 取整）一个桶，桶里的线按终点、起点（取整）各排一份。
#[derive(Default)]
struct Joints<'a> {
    /// 位置范围不大时，`lo..=hi` 里每个位置都有桶，桶号是 `位置 - lo`；否则只有 `keys` 里的位置有桶。
    lo: i64,
    dense: bool,
    keys: Vec<i64>,
    /// 桶数。
    n: usize,
    /// 第 b 个桶在 ends/starts 里占 off[b]..off[b + 1]。
    off: Vec<usize>,
    ends: Vec<(i32, u32)>,
    starts: Vec<(i32, u32)>,
    all: &'a [Rule],
}

/// 端点取整（夹到 i32：保序，只会多找到，最后还按原值判断）。
fn floor32(v: f64) -> i32 {
    v.floor().clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

impl<'a> Joints<'a> {
    /// 给 `keys` 里每个位置和它前后 `spread` 以内的位置建桶，收落在桶里的横线（或竖线）。
    fn build(all: &'a [Rule], horizontal: bool, mut keys: Vec<i64>, spread: i64) -> Self {
        let mut j = Joints {
            all,
            ..Default::default()
        };
        let (Some(&lo), Some(&hi)) = (keys.iter().min(), keys.iter().max()) else {
            return j;
        };
        let (lo, hi) = (lo - spread, hi + spread);
        j.n = if hi - lo < 1 << 16 {
            j.lo = lo;
            j.dense = true;
            (hi - lo + 1) as usize
        } else {
            keys = keys.iter().flat_map(|&k| k - spread..=k + spread).collect();
            keys.sort_unstable();
            keys.dedup();
            j.keys = keys;
            j.keys.len()
        };
        // 两遍：先数每个桶有几条，再填（不留整页的临时数组）
        let bucket_of = |q: &Rule| {
            let (o, pos, _, _) = line_of(q);
            if o == horizontal {
                j.bucket((pos * 2.0).round() as i64)
            } else {
                None
            }
        };
        let mut count = vec![0usize; j.n + 1];
        for q in all {
            if let Some(bk) = bucket_of(q) {
                count[bk + 1] += 1;
            }
        }
        for b in 1..count.len() {
            count[b] += count[b - 1];
        }
        let mut fill = count.clone();
        let mut ends = vec![(0, 0); count[j.n]];
        let mut starts = ends.clone();
        for (n, q) in all.iter().enumerate() {
            if let Some(bk) = bucket_of(q) {
                let (_, _, a, b) = line_of(q);
                ends[fill[bk]] = (floor32(b), n as u32);
                starts[fill[bk]] = (floor32(a), n as u32);
                fill[bk] += 1;
            }
        }
        (j.ends, j.starts) = (ends, starts);
        for b in 0..j.n {
            if count[b + 1] - count[b] > 1 {
                j.ends[count[b]..count[b + 1]].sort_unstable_by_key(|e| e.0);
                j.starts[count[b]..count[b + 1]].sort_unstable_by_key(|e| e.0);
            }
        }
        j.off = count;
        j
    }

    fn bucket(&self, k: i64) -> Option<usize> {
        if !self.dense {
            return self.keys.binary_search(&k).ok();
        }
        let i = usize::try_from(k.checked_sub(self.lo)?).ok()?;
        (i < self.n).then_some(i)
    }

    /// 线 `k` 上终点（`ends`）或起点取整后在 `e0..=e1` 里的线。
    fn near(&self, k: i64, ends: bool, e0: i32, e1: i32) -> &[(i32, u32)] {
        let Some(b) = self.bucket(k) else {
            return &[];
        };
        let list = if ends { &self.ends } else { &self.starts };
        let list = &list[self.off[b]..self.off[b + 1]];
        let from = list.partition_point(|e| e.0 < e0);
        let to = from + list[from..].partition_point(|e| e.0 <= e1);
        &list[from..to]
    }

    /// 和 `r`（所在位置 `at`，取整 `k`）在同一条线上（位置相差 0.5pt 内）、终点（`ends`）或起点
    /// 在 `end` 附近（取整后相差 1 内）的别的线里，有满足 `ok(起, 止)` 的。
    fn touches(
        &self,
        r: &Rule,
        k: i64,
        at: f64,
        ends: bool,
        end: f64,
        ok: impl Fn(f64, f64) -> bool,
    ) -> bool {
        let (e0, e1) = (
            floor32(end).saturating_sub(1),
            floor32(end).saturating_add(1),
        );
        (k - 1..=k + 1).any(|kk| {
            self.near(kk, ends, e0, e1).iter().any(|&(_, n)| {
                let q = &self.all[n as usize];
                let (_, qat, b0, b1) = line_of(q);
                !std::ptr::eq(q, r) && (qat - at).abs() <= 0.5 && ok(b0, b1)
            })
        })
    }
}

/// 线的方向、所在位置、沿线的起止。
fn line_of(q: &Rule) -> (bool, f64, f64, f64) {
    if q.horizontal() {
        (true, q.y0, q.x0, q.x1)
    } else {
        (false, q.x0, q.y0, q.y1)
    }
}

impl<'a> Rules<'a> {
    pub(super) fn new(rules: &'a [Rule]) -> Self {
        Rules {
            all: rules,
            sorted: OnceCell::new(),
            index: OnceCell::new(),
        }
    }

    /// (竖线按 x 排, 横线按 y 排)
    fn vh(&self) -> &(Vec<&'a Rule>, Vec<&'a Rule>) {
        self.sorted.get_or_init(|| {
            let mut v: Vec<&Rule> = self.all.iter().filter(|r| !r.horizontal()).collect();
            let mut h: Vec<&Rule> = self.all.iter().filter(|r| r.horizontal()).collect();
            v.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            h.sort_by(|a, b| a.y0.total_cmp(&b.y0));
            (v, h)
        })
    }

    fn joints(&self) -> &[Joints<'a>; 2] {
        self.index.get_or_init(|| {
            [false, true].map(|o| {
                let keys = self
                    .all
                    .iter()
                    .map(line_of)
                    .filter(|l| l.0 == o)
                    .map(|l| (l.1 * 2.0).round() as i64)
                    .collect();
                Joints::build(self.all, o, keys, 0)
            })
        })
    }

    /// 只会问 `cands` 这些短线是不是断开的一截：端点索引只给它们所在的线（和相邻的）建桶（省得给整页排序）。
    pub(super) fn prepare_joints(&self, cands: &[&Rule]) {
        let index = [false, true].map(|o| {
            let keys = cands
                .iter()
                .map(|r| line_of(r))
                .filter(|l| l.0 == o)
                .map(|l| (l.1 * 2.0).round() as i64)
                .collect();
            Joints::build(self.all, o, keys, 1)
        });
        let _ = self.index.set(index);
    }

    /// `r` 是一条线在交点处断开的中间一截：同一条线上两头都有别的线接着（空隙 1pt 内）。
    /// 只接一头的不算（刻度常常就是网格线伸出坐标轴的那一小段）。
    pub(super) fn continued(&self, r: &Rule) -> bool {
        let (o, at, a0, a1) = line_of(r);
        let k = (at * 2.0).round() as i64;
        // 同一条线：位置相差 0.5pt 内（取整后相邻的桶都查）；端点相差 1pt 内（相邻的整数桶都查）
        let joints = &self.joints()[o as usize];
        joints.touches(r, k, at, true, a0, |b0, b1| {
            b0 < a0 && (b1 - a0).abs() <= 1.0
        }) && joints.touches(r, k, at, false, a1, |b0, b1| {
            b1 > a1 && (b0 - a1).abs() <= 1.0
        })
    }

    /// 外框 `b` 的四条边里至少有三条正好贴着框线（偏差 1.5pt 内、覆盖这条边的八成以上）。
    pub(super) fn boxed(&self, b: &[f64; 4]) -> bool {
        let covered = |list: &[&Rule], at: f64, lo: f64, hi: f64, horizontal: bool| {
            if hi <= lo {
                return false;
            }
            let key = |q: &Rule| if horizontal { q.y0 } else { q.x0 };
            let from = list.partition_point(|q| key(q) < at - 1.5);
            let mut got = 0.0;
            for q in list[from..].iter().take_while(|q| key(q) <= at + 1.5) {
                let (s0, s1) = if horizontal {
                    (q.x0, q.x1)
                } else {
                    (q.y0, q.y1)
                };
                got += (s1.min(hi) - s0.max(lo)).max(0.0);
                if got >= 0.8 * (hi - lo) {
                    return true;
                }
            }
            false
        };
        let (v, h) = self.vh();
        let mut miss = 0;
        for (list, at, lo, hi, horizontal) in [
            (h, b[1], b[0], b[2], true),
            (h, b[3], b[0], b[2], true),
            (v, b[0], b[1], b[3], false),
            (v, b[2], b[1], b[3], false),
        ] {
            if !covered(list, at, lo, hi, horizontal) {
                miss += 1;
                if miss > 1 {
                    return false;
                }
            }
        }
        true
    }

    /// 外框 `b`（一行字）上下 2 个字号内都有横线盖住它（八成以上）：在表格的格子里。
    /// 图自己的网格线（落在图框 `fig` 里的横线）不算。
    pub(super) fn ruled_cell(&self, b: &[f64; 4], size: f64, fig: &[f64; 4]) -> bool {
        let own = |q: &Rule| {
            q.x0 >= fig[0] - 2.0
                && q.x1 <= fig[2] + 2.0
                && q.y0 >= fig[1] - 2.0
                && q.y0 <= fig[3] + 2.0
        };
        let covered = |from: f64, to: f64| {
            let lo = self.vh().1.partition_point(|q| q.y0 < from);
            self.vh().1[lo..]
                .iter()
                .take_while(|q| q.y0 <= to)
                .any(|q| {
                    !own(q) && (q.x1.min(b[2]) - q.x0.max(b[0])).max(0.0) >= 0.8 * (b[2] - b[0])
                })
        };
        b[2] > b[0] && covered(b[3], b[3] + 2.0 * size) && covered(b[1] - 2.0 * size, b[1])
    }
}
