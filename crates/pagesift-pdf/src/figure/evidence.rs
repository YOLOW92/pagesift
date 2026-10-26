//! 绘图证据：斜线和曲线、柱和色块、刻度，以及排除表格的格子底色、隔行底色、框线交点。

use super::cluster::Item;
use super::rules::Rules;
use super::{MAX_PATH_WEIGHT, MIN_TICKS, TICK_LEN, center};
use crate::geom::{Rule, Shape};
use crate::layout::Line;

/// 按 y 排好的文字行（中心、横向范围、字号），用来快速数一个框里有几行字、找紧挨着图形的字。
pub(super) struct Centers {
    pts: Vec<(f64, f64, f64, f64, f64)>,
}

impl Centers {
    pub(super) fn new(lines: &[Line]) -> Self {
        let mut pts: Vec<(f64, f64, f64, f64, f64)> = lines
            .iter()
            .map(|l| {
                let (x, y) = center(l);
                (x, y, l.x0, l.x1, l.size)
            })
            .collect();
        pts.sort_by(|a, b| a.1.total_cmp(&b.1));
        Centers { pts }
    }

    pub(super) fn count(&self, b: &[f64; 4]) -> usize {
        let lo = self.pts.partition_point(|p| p.1 < b[1]);
        self.pts[lo..]
            .iter()
            .take_while(|p| p.1 <= b[3])
            .filter(|p| p.0 >= b[0] && p.0 <= b[2])
            .count()
    }

    /// 有没有正文样的长行（行宽不小于 15 个字号）穿过外框 `b`。
    fn has_body_line(&self, b: &[f64; 4]) -> bool {
        let lo = self.pts.partition_point(|p| p.1 < b[1]);
        self.pts[lo..]
            .iter()
            .take_while(|p| p.1 <= b[3])
            .any(|&(_, _, x0, x1, s)| x0 <= b[2] && x1 >= b[0] && x1 - x0 >= 15.0 * s)
    }

    /// 和外框 `b` 同一行（字的中心在框的上下范围附近）、横向紧挨着（空隙不超过一个字号）的字。
    fn beside(&self, b: &[f64; 4]) -> bool {
        let lo = self.pts.partition_point(|p| p.1 < b[1] - 12.0);
        self.pts[lo..]
            .iter()
            .take_while(|p| p.1 <= b[3] + 12.0)
            .any(|&(_, cy, x0, x1, s)| {
                cy >= b[1] - 0.5 * s && cy <= b[3] + 0.5 * s && (x0 - b[2]).max(b[0] - x1) <= s
            })
    }
}

/// 绘图证据：每条带权重的外框。
pub(super) fn evidence<'a>(
    shapes: &[Shape],
    rules: &'a [Rule],
    index: &Rules<'a>,
    centers: &Centers,
    page: (f64, f64),
) -> Vec<Item> {
    let mut items = Vec::new();
    for s in shapes.iter().filter(|s| s.visible) {
        let b = [
            s.x0.max(0.0),
            s.y0.max(0.0),
            s.x1.min(page.0),
            s.y1.min(page.1),
        ];
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        if !(w.is_finite() && h.is_finite())
            || w <= 0.0 && h <= 0.0
            || w * h > 0.25 * page.0 * page.1
        {
            continue;
        }
        let bent = u32::from(s.slanted) + u32::from(s.curves);
        let (weight, bar) = if bent > 0 && !s.rect {
            // 框住文字的圆角框是文本框，不是图；流程图里装一两行短字的小节点框算图
            if s.curves <= 8 && s.slanted <= 4 && w > 20.0 && h > 12.0 {
                let n = centers.count(&b);
                if n >= 3 || (n > 0 && w * h > 150.0 * 60.0) {
                    continue;
                }
            }
            // 压在正文上的（水印字形、背景花纹）不是图
            if centers.has_body_line(&b) {
                continue;
            }
            // 行内图标（表格里数字旁的小箭头）：很小、紧挨着一行字
            if w.max(h) <= 16.0 && centers.beside(&b) {
                continue;
            }
            // 表格里表示“不适用”的斜线：外框有三条以上的边正好贴着框线
            if s.curves == 0 && index.boxed(&b) {
                continue;
            }
            (bent.min(MAX_PATH_WEIGHT), false)
        } else if s.fill && w > 2.5 && h > 2.5 && centers.count(&b) == 0 {
            // 柱、图例色块（竖横线围成的填充多边形也算）
            (0, true)
        } else {
            continue;
        };
        items.push(Item { b, w: weight, bar });
    }
    drop_table_fills(&mut items, centers);
    // 刻度：同一条横轴上的短竖线、同一条纵轴上的短横线，至少三根；长线断开的一截（表格框线的交点）不算
    // 矩形的边（隔行底色两头的竖边）也不算
    let q = |v: f64| (v * 2.0).round() as i64;
    let short = |r: &Rule| (1.5..=TICK_LEN).contains(&(r.x1 - r.x0).max(r.y1 - r.y0));
    let vc = paired(
        rules
            .iter()
            .filter(|r| !r.horizontal() && short(r))
            .collect(),
        |r| r.y0,
    );
    let hc = paired(
        rules
            .iter()
            .filter(|r| r.horizontal() && short(r))
            .collect(),
        |r| r.x0,
    );
    // 候选短线是矩形的边就不算：候选的键排好，扫矩形（只看有短边的）时标出哪些键是矩形的边
    let key = |r: &Rule| {
        if r.horizontal() {
            (true, q(r.y0), q(r.x0), q(r.x1))
        } else {
            (false, q(r.x0), q(r.y0), q(r.y1))
        }
    };
    let mut keys: Vec<(bool, i64, i64, i64)> = vc.iter().chain(hc.iter()).map(|r| key(r)).collect();
    keys.sort_unstable();
    keys.dedup();
    let mut edge = vec![false; keys.len()];
    if !keys.is_empty() {
        let mut mark = |k: (bool, i64, i64, i64)| {
            if let Ok(i) = keys.binary_search(&k) {
                edge[i] = true;
            }
        };
        for s in shapes.iter().filter(|s| s.rect) {
            if s.y1 - s.y0 <= TICK_LEN + 0.5 {
                for x in [s.x0, s.x1] {
                    mark((false, q(x), q(s.y0), q(s.y1)));
                }
            }
            if s.x1 - s.x0 <= TICK_LEN + 0.5 {
                for y in [s.y0, s.y1] {
                    mark((true, q(y), q(s.x0), q(s.x1)));
                }
            }
        }
        index.prepare_joints(&vc.iter().chain(hc.iter()).copied().collect::<Vec<_>>());
    }
    let tick =
        |r: &&Rule| !keys.binary_search(&key(r)).is_ok_and(|i| edge[i]) && !index.continued(r);
    let mut vt: Vec<&Rule> = vc.into_iter().filter(tick).collect();
    let mut ht: Vec<&Rule> = hc.into_iter().filter(tick).collect();
    vt.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
    ht.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.y0.total_cmp(&b.y0)));
    ticks(&vt, |r| r.y0, &mut items);
    ticks(&ht, |r| r.x0, &mut items);
    items
}

/// 成排才可能是刻度。按另一方向的位置 `key` 排好，相邻相差 1pt 内的连成一串：分组时组员和组首相差 1pt 内，
/// 组不会跨串，各串怎么分组互不影响；不到 MIN_TICKS 根的串成不了刻度，整串跳过，结果不变。
fn paired(mut v: Vec<&Rule>, key: impl Fn(&Rule) -> f64) -> Vec<&Rule> {
    v.sort_by(|a, b| key(a).total_cmp(&key(b)));
    let mut keep = Vec::new();
    for run in v.chunk_by(|a, b| key(b) - key(a) <= 1.0) {
        if run.len() >= MIN_TICKS {
            keep.extend_from_slice(run);
        }
    }
    keep
}

/// 排好的短线（`ts`）从头分组：和组里第一根的 `key` 相差 1pt 内的同组，至少 MIN_TICKS 根的一组是刻度。
fn ticks(ts: &[&Rule], key: impl Fn(&Rule) -> f64, items: &mut Vec<Item>) {
    let mut i = 0;
    while i < ts.len() {
        let mut j = i + 1;
        while j < ts.len() && (key(ts[j]) - key(ts[i])).abs() <= 1.0 {
            j += 1;
        }
        if j - i >= MIN_TICKS {
            for r in &ts[i..j] {
                items.push(Item {
                    b: [r.x0, r.y0, r.x1, r.y1],
                    w: 1,
                    bar: false,
                });
            }
        }
        i = j;
    }
}

/// 表格的填充不是柱：
/// - 同样大小、排成一列（或一行）的三块以上（隔行底色、单元格的留白填充、图例色块）；
/// - 左右相接、上下边一样（或上下相接、左右边一样）的几块连成一条，条里有字（表头、整行的底色）。
fn drop_table_fills(items: &mut Vec<Item>, centers: &Centers) {
    fn q(v: f64) -> i64 {
        (v * 2.0).round() as i64
    }
    let bars: Vec<usize> = (0..items.len()).filter(|&k| items[k].bar).collect();
    if bars.is_empty() {
        return;
    }
    let mut drop = vec![false; items.len()];
    // 按键排好，键相同的一段是一组
    let groups = |key: &dyn Fn(&[f64; 4]) -> [i64; 3]| {
        let mut v: Vec<([i64; 3], usize)> = bars.iter().map(|&k| (key(&items[k].b), k)).collect();
        v.sort_unstable();
        v
    };
    for key in [
        (|b: &[f64; 4]| [q(b[2] - b[0]), q(b[3] - b[1]), q(b[0])]) as fn(&[f64; 4]) -> [i64; 3],
        |b| [q(b[2] - b[0]), q(b[3] - b[1]), q(b[1])],
    ] {
        for g in groups(&key).chunk_by(|a, b| a.0 == b.0) {
            if g.len() >= 3 {
                for &(_, k) in g {
                    drop[k] = true;
                }
            }
        }
    }
    // 相接成条：按左右相接（上下边相同）、上下相接（左右边相同）并查集
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    // 组内按起点排（键的第三项），相邻两块首尾相接就并
    for (key, lo, hi) in [
        (
            (|b: &[f64; 4]| [q(b[1]), q(b[3]), 0]) as fn(&[f64; 4]) -> [i64; 3],
            0,
            2,
        ),
        (|b| [q(b[0]), q(b[2]), 0], 1, 3),
    ] {
        let mut v = groups(&key);
        for g in v.chunk_by_mut(|a, b| a.0 == b.0) {
            g.sort_by(|a, b| items[a.1].b[lo].total_cmp(&items[b.1].b[lo]));
            for w in g.windows(2) {
                if (items[w[1].1].b[lo] - items[w[0].1].b[hi]).abs() <= 1.0 {
                    let (a, b) = (find(&mut parent, w[0].1), find(&mut parent, w[1].1));
                    parent[a] = b;
                }
            }
        }
    }
    let mut bands: Vec<(usize, usize)> = bars.iter().map(|&k| (find(&mut parent, k), k)).collect();
    bands.sort_unstable();
    for g in bands.chunk_by(|a, b| a.0 == b.0) {
        if g.len() < 2 {
            continue;
        }
        let mut b = items[g[0].1].b;
        for &(_, k) in &g[1..] {
            super::union(&mut b, &items[k].b);
        }
        if centers.count(&b) > 0 {
            for &(_, k) in g {
                drop[k] = true;
            }
        }
    }
    let mut it = drop.iter();
    items.retain(|_| !it.next().copied().unwrap_or(false));
}
