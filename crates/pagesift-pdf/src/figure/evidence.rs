//! 绘图证据：斜线和曲线、柱和色块、刻度，以及排除表格的格子底色、隔行底色、框线交点。

use std::collections::{HashMap, HashSet};

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
pub(super) fn evidence(
    shapes: &[Shape],
    rules: &[Rule],
    index: &Rules,
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
        let bent = s.slanted + s.curves;
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
            if s.curves == 0 && index.boxed(&b) >= 3 {
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
    let mut edges: HashSet<(bool, i64, i64, i64)> = HashSet::new();
    for s in shapes.iter().filter(|s| s.rect) {
        for x in [s.x0, s.x1] {
            edges.insert((false, q(x), q(s.y0), q(s.y1)));
        }
        for y in [s.y0, s.y1] {
            edges.insert((true, q(y), q(s.x0), q(s.x1)));
        }
    }
    let tick = |r: &&Rule| {
        let len = (r.x1 - r.x0).max(r.y1 - r.y0);
        let edge = if r.horizontal() {
            (true, q(r.y0), q(r.x0), q(r.x1))
        } else {
            (false, q(r.x0), q(r.y0), q(r.y1))
        };
        (1.5..=TICK_LEN).contains(&len) && !index.continued(r) && !edges.contains(&edge)
    };
    let mut vt: Vec<&Rule> = rules
        .iter()
        .filter(|r| !r.horizontal())
        .filter(tick)
        .collect();
    let mut ht: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.horizontal())
        .filter(tick)
        .collect();
    vt.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
    ht.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.y0.total_cmp(&b.y0)));
    for (ts, key) in [
        (&vt, (|r: &Rule| r.y0) as fn(&Rule) -> f64),
        (&ht, |r: &Rule| r.x0),
    ] {
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
    items
}

/// 表格的填充不是柱：
/// - 同样大小、排成一列（或一行）的三块以上（隔行底色、单元格的留白填充、图例色块）；
/// - 左右相接、上下边一样（或上下相接、左右边一样）的几块连成一条，条里有字（表头、整行的底色）。
fn drop_table_fills(items: &mut Vec<Item>, centers: &Centers) {
    let q = |v: f64| (v * 2.0).round() as i64;
    let bars: Vec<usize> = (0..items.len()).filter(|&k| items[k].bar).collect();
    if bars.is_empty() {
        return;
    }
    let mut drop = vec![false; items.len()];
    let size = |b: &[f64; 4]| (q(b[2] - b[0]), q(b[3] - b[1]));
    let mut by_col: HashMap<((i64, i64), i64), Vec<usize>> = HashMap::new();
    let mut by_row: HashMap<((i64, i64), i64), Vec<usize>> = HashMap::new();
    for &k in &bars {
        let b = items[k].b;
        by_col.entry((size(&b), q(b[0]))).or_default().push(k);
        by_row.entry((size(&b), q(b[1]))).or_default().push(k);
    }
    for g in by_col.values().chain(by_row.values()) {
        if g.len() >= 3 {
            for &k in g {
                drop[k] = true;
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
    let mut rows: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut cols: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for &k in &bars {
        let b = items[k].b;
        rows.entry((q(b[1]), q(b[3]))).or_default().push(k);
        cols.entry((q(b[0]), q(b[2]))).or_default().push(k);
    }
    for (groups, lo, hi) in [(&mut rows, 0, 2), (&mut cols, 1, 3)] {
        for g in groups.values_mut() {
            g.sort_by(|&a, &b| items[a].b[lo].total_cmp(&items[b].b[lo]));
            for w in g.windows(2) {
                if (items[w[1]].b[lo] - items[w[0]].b[hi]).abs() <= 1.0 {
                    let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
                    parent[a] = b;
                }
            }
        }
    }
    let mut bands: HashMap<usize, ([f64; 4], Vec<usize>)> = HashMap::new();
    for &k in &bars {
        let r = find(&mut parent, k);
        let e = bands.entry(r).or_insert((items[k].b, Vec::new()));
        super::union(&mut e.0, &items[k].b);
        e.1.push(k);
    }
    for (b, ks) in bands.values() {
        if ks.len() >= 2 && centers.count(b) > 0 {
            for &k in ks {
                drop[k] = true;
            }
        }
    }
    let mut it = drop.iter();
    items.retain(|_| !it.next().copied().unwrap_or(false));
}
