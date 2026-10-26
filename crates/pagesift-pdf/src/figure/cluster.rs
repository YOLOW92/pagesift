//! 绘图证据聚类：按距离、按柱的排列、按同时碰到两类的坐标轴线把证据并成一张张图。

use std::collections::HashMap;

use super::{GAP, MERGE_GAP, near, union};

/// 一页最多拿这么多根柱两两比较排列。
const MAX_BARS: usize = 2000;
use crate::geom::Rule;

#[derive(Clone, Copy)]
pub(super) struct Item {
    pub(super) b: [f64; 4],
    pub(super) w: u32,
    /// 填充矩形（柱、色块）；单独看它们不可靠（表格里放照片的格子底色也是），要看排列。
    pub(super) bar: bool,
}

/// 一类证据：外框、线和刻度的权重、柱的外框。
pub(super) struct Cluster {
    pub(super) b: [f64; 4],
    pub(super) w: u32,
    pub(super) bars: Vec<[f64; 4]>,
}

impl Cluster {
    fn absorb(&mut self, o: Cluster) {
        union(&mut self.b, &o.b);
        self.w += o.w;
        self.bars.extend(o.bars);
    }

    /// 权重：线和刻度照算；柱要像柱形图（至少三个不同的起点、三种不同的长度）才算，
    /// 同一列里上下相接、宽度一样的格子底色不算。
    pub(super) fn weight(&self) -> u32 {
        let distinct = |f: &dyn Fn(&[f64; 4]) -> f64| {
            let mut v: Vec<i64> = self.bars.iter().map(|b| f(b).round() as i64).collect();
            v.sort_unstable();
            v.dedup();
            v.len()
        };
        let vertical = distinct(&|b| b[0]) >= 3 && distinct(&|b| b[3] - b[1]) >= 3;
        let horizontal = distinct(&|b| b[1]) >= 3 && distinct(&|b| b[2] - b[0]) >= 3;
        let bars = if vertical || horizontal {
            self.bars.len() as u32
        } else {
            0
        };
        self.w + bars
    }
}

/// 证据按距离聚类（网格分桶的并查集），再把相近的类合并；返回 (外框, 权重)。
pub(super) fn cluster(items: &[Item], rules: &[Rule]) -> Vec<Cluster> {
    const CELL: f64 = 24.0;
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut buckets: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (k, it) in items.iter().enumerate() {
        let b = it.b;
        let (cx0, cy0) = (
            ((b[0] - GAP) / CELL).floor() as i64,
            ((b[1] - GAP) / CELL).floor() as i64,
        );
        let (cx1, cy1) = (
            ((b[2] + GAP) / CELL).floor() as i64,
            ((b[3] + GAP) / CELL).floor() as i64,
        );
        // 很大的外框不进桶（免得占满整页的桶），留给后面的类间合并
        if (cx1 - cx0 + 1) * (cy1 - cy0 + 1) > 400 {
            continue;
        }
        for cx in cx0..=cx1 {
            for cy in cy0..=cy1 {
                let v = buckets.entry((cx, cy)).or_default();
                for &o in v.iter() {
                    if near(&items[o].b, &b, GAP) {
                        let (ra, rb) = (find(&mut parent, o), find(&mut parent, k));
                        if ra != rb {
                            parent[ra] = rb;
                        }
                    }
                }
                v.push(k);
            }
        }
    }
    // 柱形图的柱可能隔得很开：同宽、上下范围重叠、间距不超过 4 个柱宽（横柱同理）的柱先并成一组
    let bars: Vec<usize> = (0..items.len()).filter(|&k| items[k].bar).collect();
    if bars.len() <= MAX_BARS {
        for (n, &a) in bars.iter().enumerate() {
            for &b in &bars[n + 1..] {
                if same_series(&items[a].b, &items[b].b) {
                    let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                    if ra != rb {
                        parent[ra] = rb;
                    }
                }
            }
        }
    }
    let mut groups: HashMap<usize, Cluster> = HashMap::new();
    for (k, it) in items.iter().enumerate() {
        let r = find(&mut parent, k);
        let one = Cluster {
            b: it.b,
            w: it.w,
            bars: if it.bar { vec![it.b] } else { Vec::new() },
        };
        match groups.get_mut(&r) {
            Some(c) => c.absorb(one),
            None => {
                groups.insert(r, one);
            }
        }
    }
    let mut boxes: Vec<Cluster> = groups.into_values().collect();
    // 并查集的根来自 HashMap，顺序不定：按权重和外框排成确定的顺序（合并结果和认领顺序都依赖它）
    boxes.sort_by(|a, b| {
        (b.w + b.bars.len() as u32)
            .cmp(&(a.w + a.bars.len() as u32))
            .then(a.b[0].total_cmp(&b.b[0]))
            .then(a.b[1].total_cmp(&b.b[1]))
            .then(a.b[2].total_cmp(&b.b[2]))
            .then(a.b[3].total_cmp(&b.b[3]))
    });
    // 相近的类合并（同一张图里离得稍远的柱、图例）；类不多，直接两两比
    if boxes.len() > 2000 {
        boxes.truncate(2000);
    }
    merge_near(&mut boxes);
    // 坐标轴线同时碰到两类（纵轴的刻度和柱、两组柱）就把它们并起来；线不能伸出这两类合起来的范围，
    // 免得整页的分隔线把上下两张图连成一张
    let long: Vec<&Rule> = rules
        .iter()
        .filter(|r| (r.x1 - r.x0).max(r.y1 - r.y0) >= 10.0)
        .collect();
    if boxes.len() > 1 && boxes.len() <= 400 {
        for r in long {
            let rb = [r.x0, r.y0, r.x1, r.y1];
            let hit: Vec<usize> = (0..boxes.len())
                .filter(|&i| near(&boxes[i].b, &rb, GAP))
                .collect();
            if hit.len() < 2 {
                continue;
            }
            let mut u = boxes[hit[0]].b;
            for &i in &hit[1..] {
                union(&mut u, &boxes[i].b);
            }
            if rb[0] < u[0] - GAP || rb[2] > u[2] + GAP || rb[1] < u[1] - GAP || rb[3] > u[3] + GAP
            {
                continue;
            }
            for &i in hit[1..].iter().rev() {
                let o = boxes.swap_remove(i);
                boxes[hit[0]].absorb(o);
            }
        }
        merge_near(&mut boxes);
    }
    boxes
}

/// 两根柱是同一组：竖柱同宽、上下范围重叠、横向间距不超过 4 个柱宽且不超过 60pt；横柱同理。
fn same_series(a: &[f64; 4], b: &[f64; 4]) -> bool {
    let (wa, wb, ha, hb) = (a[2] - a[0], b[2] - b[0], a[3] - a[1], b[3] - b[1]);
    let gap_x = (b[0] - a[2]).max(a[0] - b[2]);
    let gap_y = (b[1] - a[3]).max(a[1] - b[3]);
    let vertical =
        (wa - wb).abs() <= 0.5 && gap_y < 0.0 && gap_x >= 0.0 && gap_x <= (4.0 * wa).min(60.0);
    let horizontal =
        (ha - hb).abs() <= 0.5 && gap_x < 0.0 && gap_y >= 0.0 && gap_y <= (4.0 * ha).min(60.0);
    vertical || horizontal
}

/// 相近的类合并，直到没有可并的。
fn merge_near(boxes: &mut Vec<Cluster>) {
    loop {
        let mut merged = false;
        let mut i = 0;
        while i < boxes.len() {
            let mut j = i + 1;
            while j < boxes.len() {
                if near(&boxes[i].b, &boxes[j].b, MERGE_GAP) {
                    let o = boxes.swap_remove(j);
                    boxes[i].absorb(o);
                    merged = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
        if !merged {
            break;
        }
    }
}
