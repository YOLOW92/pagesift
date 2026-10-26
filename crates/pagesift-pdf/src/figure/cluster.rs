//! 绘图证据聚类：按距离、按柱的排列、按同时碰到两类的坐标轴线把证据并成一张张图。

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
    // 最终的类是“外框相距不超过 MERGE_GAP 就合并”的不动点；证据之间 GAP 以内的相近被它包含，
    // 不必逐对比较证据（一页几万条铺满绘图区的曲线时逐对比较很慢），直接从单条证据开始合并。
    let mut parent: Vec<usize> = (0..items.len()).collect();
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
    // 中心落在同一个 8pt 格子里的证据外框相距不到 8pt，最后一定会并到一起：先并起来，少做几轮合并
    // （柱不先并，柱形图的判断要看每根柱）
    let keys: Vec<(i64, i64)> = items
        .iter()
        .map(|it| {
            let b = it.b;
            (
                ((b[0] + b[2]) / 16.0).floor() as i64,
                ((b[1] + b[3]) / 16.0).floor() as i64,
            )
        })
        .collect();
    let solid: Vec<usize> = (0..items.len()).filter(|&k| !items[k].bar).collect();
    let (x0, x1) = solid
        .iter()
        .map(|&k| keys[k].0)
        .fold((i64::MAX, i64::MIN), |(a, b), x| (a.min(x), b.max(x)));
    let (y0, y1) = solid
        .iter()
        .map(|&k| keys[k].1)
        .fold((i64::MAX, i64::MIN), |(a, b), y| (a.min(y), b.max(y)));
    let mut join = |a: usize, b: usize| {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    };
    let (w, h) = (
        x1.saturating_sub(x0).saturating_add(1),
        y1.saturating_sub(y0).saturating_add(1),
    );
    if !solid.is_empty() && w.saturating_mul(h) <= 1 << 16 {
        // 格子不多：每格记第一条证据，同格的并过去
        let mut first = vec![usize::MAX; (w * h) as usize];
        for &k in &solid {
            let c = ((keys[k].0 - x0) * h + keys[k].1 - y0) as usize;
            if first[c] == usize::MAX {
                first[c] = k;
            } else {
                join(first[c], k);
            }
        }
    } else {
        let mut order: Vec<((i64, i64), usize)> = solid.iter().map(|&k| (keys[k], k)).collect();
        order.sort_unstable();
        for p in order.windows(2) {
            if p[0].0 == p[1].0 {
                join(p[0].1, p[1].1);
            }
        }
    }
    let mut slot: Vec<usize> = vec![usize::MAX; items.len()];
    let mut boxes: Vec<Cluster> = Vec::new();
    for (k, it) in items.iter().enumerate() {
        let r = find(&mut parent, k);
        let one = Cluster {
            b: it.b,
            w: it.w,
            bars: if it.bar { vec![it.b] } else { Vec::new() },
        };
        if slot[r] == usize::MAX {
            slot[r] = boxes.len();
            boxes.push(one);
        } else {
            boxes[slot[r]].absorb(one);
        }
    }
    // 按权重和外框排成确定的顺序（合并结果和认领顺序都依赖它）
    boxes.sort_by(|a, b| {
        (b.w + b.bars.len() as u32)
            .cmp(&(a.w + a.bars.len() as u32))
            .then(a.b[0].total_cmp(&b.b[0]))
            .then(a.b[1].total_cmp(&b.b[1]))
            .then(a.b[2].total_cmp(&b.b[2]))
            .then(a.b[3].total_cmp(&b.b[3]))
    });
    // 相近的类合并（同一张图里离得稍远的柱、图例）；类不多，直接两两比
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
            let mut hits = (0..boxes.len()).filter(|&i| near(&boxes[i].b, &rb, GAP));
            let (Some(h0), Some(h1)) = (hits.next(), hits.next()) else {
                continue;
            };
            let hit: Vec<usize> = [h0, h1].into_iter().chain(hits).collect();
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

/// 相近的类合并，直到没有可并的。两类一旦相距不超过 MERGE_GAP，之后各自只会变大，迟早并到一起，
/// 所以结果和合并的先后无关：外框放进网格，一个类的外框变大就补登记新占的格子、再拿它找邻居。
/// 并完按每类最早的成员排回原来的先后。
fn merge_near(boxes: &mut Vec<Cluster>) {
    let n = boxes.len();
    if n < 2 {
        return;
    }
    let mut b: Vec<[f64; 4]> = boxes.iter().map(|c| c.b).collect();
    let mut grid = Grid::new(&b);
    for (i, bi) in b.iter().enumerate() {
        grid.register(i, bi);
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut stack: Vec<usize> = (0..n).rev().collect();
    let mut cand = Vec::new();
    while let Some(i) = stack.pop() {
        if parent[i] != i {
            continue;
        }
        grid.candidates(i, &mut cand);
        let mut grew = false;
        for &c in &cand {
            let rc = find(&mut parent, c);
            if rc != i && near(&b[i], &b[rc], MERGE_GAP) {
                parent[rc] = i;
                let o = b[rc];
                union(&mut b[i], &o);
                grew = true;
            }
        }
        if grew {
            grid.register(i, &b[i]);
            stack.push(i);
        }
    }
    let mut slot: Vec<usize> = vec![usize::MAX; n];
    let mut out: Vec<Cluster> = Vec::new();
    for (i, c) in std::mem::take(boxes).into_iter().enumerate() {
        let r = find(&mut parent, i);
        if slot[r] == usize::MAX {
            slot[r] = out.len();
            out.push(c);
        } else {
            out[slot[r]].absorb(c);
        }
    }
    *boxes = out;
}

/// 合并用的网格：每个类登记在它的外框（四周放宽 MERGE_GAP）占的格子里；很大的外框不登记，直接和所有类比。
/// 网格每边最多 MAX_GRID 格，越界的格子夹到边上（夹取保序，相交的范围夹完仍相交）。
struct Grid {
    x0: i64,
    y0: i64,
    w: i64,
    h: i64,
    cells: Vec<Vec<usize>>,
    big: Vec<usize>,
    /// 已登记的格子范围；None 是还没登记或是大外框。
    reg: Vec<Option<[i64; 4]>>,
    is_big: Vec<bool>,
}

impl Grid {
    const CELL: f64 = 32.0;
    const MAX_CELLS: i64 = 400;
    const MAX_GRID: i64 = 256;

    fn span(b: &[f64; 4]) -> [i64; 4] {
        let f = |v: f64| (v / Self::CELL).floor() as i64;
        [
            f(b[0] - MERGE_GAP),
            f(b[1] - MERGE_GAP),
            f(b[2] + MERGE_GAP),
            f(b[3] + MERGE_GAP),
        ]
    }

    fn new(boxes: &[[f64; 4]]) -> Self {
        let spans: Vec<[i64; 4]> = boxes.iter().map(Self::span).collect();
        let x0 = spans.iter().map(|s| s[0]).min().unwrap_or(0);
        let y0 = spans.iter().map(|s| s[1]).min().unwrap_or(0);
        let x1 = spans.iter().map(|s| s[2]).max().unwrap_or(0);
        let y1 = spans.iter().map(|s| s[3]).max().unwrap_or(0);
        let (w, h) = (
            (x1.saturating_sub(x0) + 1).clamp(1, Self::MAX_GRID),
            (y1.saturating_sub(y0) + 1).clamp(1, Self::MAX_GRID),
        );
        Grid {
            x0,
            y0,
            w,
            h,
            cells: vec![Vec::new(); (w * h) as usize],
            big: Vec::new(),
            reg: vec![None; boxes.len()],
            is_big: vec![false; boxes.len()],
        }
    }

    fn register(&mut self, i: usize, b: &[f64; 4]) {
        if self.is_big[i] {
            return;
        }
        let s = Self::span(b);
        if (s[2] - s[0] + 1).saturating_mul(s[3] - s[1] + 1) > Self::MAX_CELLS {
            self.is_big[i] = true;
            self.big.push(i);
            return;
        }
        let cx = |v: i64| v.saturating_sub(self.x0).clamp(0, self.w - 1);
        let cy = |v: i64| v.saturating_sub(self.y0).clamp(0, self.h - 1);
        let c = [cx(s[0]), cy(s[1]), cx(s[2]), cy(s[3])];
        let old = self.reg[i];
        for x in c[0]..=c[2] {
            for y in c[1]..=c[3] {
                if old.is_some_and(|o| x >= o[0] && x <= o[2] && y >= o[1] && y <= o[3]) {
                    continue;
                }
                self.cells[(y * self.w + x) as usize].push(i);
            }
        }
        self.reg[i] = Some(c);
    }

    /// 可能和类 `i` 相近的类（可能有重复、已并掉的）。
    fn candidates(&self, i: usize, out: &mut Vec<usize>) {
        out.clear();
        if self.is_big[i] {
            out.extend(0..self.reg.len());
            return;
        }
        if let Some(c) = self.reg[i] {
            for x in c[0]..=c[2] {
                for y in c[1]..=c[3] {
                    out.extend(&self.cells[(y * self.w + x) as usize]);
                }
            }
        }
        out.extend(&self.big);
    }
}

fn find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}
