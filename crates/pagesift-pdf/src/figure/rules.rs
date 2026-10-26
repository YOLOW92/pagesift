//! 横竖线的查询：短线是不是长线断开的一截（表格框线在交点处常断开），一个外框有几条边贴着框线（表格格子）。

use crate::geom::Rule;

/// 按位置排好的横线、竖线。
pub(super) struct Rules<'a> {
    /// 竖线按 x 排。
    v: Vec<&'a Rule>,
    /// 横线按 y 排。
    h: Vec<&'a Rule>,
}

impl<'a> Rules<'a> {
    pub(super) fn new(rules: &'a [Rule]) -> Self {
        let mut v: Vec<&Rule> = rules.iter().filter(|r| !r.horizontal()).collect();
        let mut h: Vec<&Rule> = rules.iter().filter(|r| r.horizontal()).collect();
        v.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        h.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        Rules { v, h }
    }

    /// 同方向、同一条线上（偏差 0.5pt 内）的线，按位置取出。
    fn along(&self, r: &Rule) -> &[&'a Rule] {
        let (list, key, at): (&[&Rule], fn(&Rule) -> f64, f64) = if r.horizontal() {
            (&self.h, |q| q.y0, r.y0)
        } else {
            (&self.v, |q| q.x0, r.x0)
        };
        let lo = list.partition_point(|q| key(q) < at - 0.5);
        let hi = list.partition_point(|q| key(q) <= at + 0.5);
        &list[lo..hi]
    }

    /// `r` 是一条线在交点处断开的中间一截：同一条线上两头都有别的线接着（空隙 1pt 内）。
    /// 只接一头的不算（刻度常常就是网格线伸出坐标轴的那一小段）。
    pub(super) fn continued(&self, r: &Rule) -> bool {
        let span = |q: &Rule| {
            if q.horizontal() {
                (q.x0, q.x1)
            } else {
                (q.y0, q.y1)
            }
        };
        let (a0, a1) = span(r);
        let (mut before, mut after) = (false, false);
        for q in self.along(r) {
            if std::ptr::eq(*q, r) {
                continue;
            }
            let (b0, b1) = span(q);
            before |= b0 < a0 && (b1 - a0).abs() <= 1.0;
            after |= b1 > a1 && (b0 - a1).abs() <= 1.0;
        }
        before && after
    }

    /// 外框 `b` 的四条边里有几条正好贴着框线（偏差 1.5pt 内、覆盖这条边的八成以上）。
    pub(super) fn boxed(&self, b: &[f64; 4]) -> usize {
        let covered = |list: &[&Rule], at: f64, lo: f64, hi: f64, horizontal: bool| {
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
            }
            hi > lo && got >= 0.8 * (hi - lo)
        };
        [
            covered(&self.h, b[1], b[0], b[2], true),
            covered(&self.h, b[3], b[0], b[2], true),
            covered(&self.v, b[0], b[1], b[3], false),
            covered(&self.v, b[2], b[1], b[3], false),
        ]
        .iter()
        .filter(|&&c| c)
        .count()
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
            let lo = self.h.partition_point(|q| q.y0 < from);
            self.h[lo..].iter().take_while(|q| q.y0 <= to).any(|q| {
                !own(q) && (q.x1.min(b[2]) - q.x0.max(b[0])).max(0.0) >= 0.8 * (b[2] - b[0])
            })
        };
        b[2] > b[0] && covered(b[3], b[3] + 2.0 * size) && covered(b[1] - 2.0 * size, b[1])
    }
}
