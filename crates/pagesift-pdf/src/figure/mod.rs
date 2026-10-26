//! 矢量图区域：坐标轴、刻度、折线、柱、饼、图例色块这些绘图路径聚成的区域，以及挂在上面的文字
//! （刻度值、轴标题、图例、图内标注）。
//!
//! 思路参考 pdffigures2（先找图形元素，再把图形旁边“不像正文”的文字归给图），按自己的规则写：
//! 1. 证据（evidence.rs）：斜线和曲线（折线、饼、圆点、流程图节点）、里面没有文字的填充矩形（柱、色块）、
//!    成排的短刻度线。不算的：白色的填充和描边、框住多行字的文本框、压在正文上的水印、紧挨着字的行内图标、
//!    表格里的斜线格、隔行底色和单元格留白（同样大小排成一列）、连成条且条里有字的底色、框线交点的断头、矩形的边。
//! 2. 聚类（cluster.rs）：按距离聚；同宽（或同高）、范围重叠的柱先并成一组；一条坐标轴线同时碰到两类、
//!    又不伸出它们的范围时把两类并起来。柱要像柱形图（起点、长度各有三种以上）才算权重。
//!    权重够、面积够的类是图，再并上贴着它的横竖线（坐标轴、网格线）。
//! 3. 认领文字：图框里的字归图；图框外紧贴着的短标签（刻度值、图例、单位）逐轮吸进来。
//!    题注（`Figure 3`、`図表2`）、正文样的长句、大字、页眉页脚区的字、写入方声明的标题（结构树 H、书签）、
//!    表格格子里的字不吸，它们也挡住后面的吸附。图框里正文样的行太多（文本框、整栏背景）就不是图。
//!
//! 图内文字不参加表格检测，合成一个原样输出的块，不当标题。

mod cluster;
mod evidence;
mod rules;
mod text;

use crate::geom::{Rule, Shape};
use crate::layout::Line;
use cluster::{Cluster, cluster};
use evidence::{Centers, evidence};
use rules::Rules;
use text::Feats;
#[cfg(test)]
use text::{caption_like, label_like, prose_like};

/// 聚类时证据之间允许的空隙（pt）。
pub(super) const GAP: f64 = 8.0;
/// 类与类之间再合并的空隙（pt）。
pub(super) const MERGE_GAP: f64 = 12.0;
/// 成为图的最小权重、最小宽高（pt）。
const MIN_WEIGHT: u32 = 6;
const MIN_W: f64 = 40.0;
const MIN_H: f64 = 30.0;
/// 刻度线最长多少（pt），同一条轴上至少几根。
pub(super) const TICK_LEN: f64 = 10.0;
pub(super) const MIN_TICKS: usize = 3;
/// 单条路径最多算多少权重（一条折线几百段也只算这么多）。
pub(super) const MAX_PATH_WEIGHT: u32 = 20;

/// 一个图区：外框（y 向上）和归它的文字行在输入里的序号。
#[derive(Debug, Clone)]
pub(crate) struct Figure {
    pub(crate) x0: f64,
    pub(crate) bottom: f64,
    pub(crate) x1: f64,
    pub(crate) top: f64,
    pub(crate) lines: Vec<usize>,
}

impl Figure {
    pub(crate) fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.bottom && y <= self.top
    }
}

/// 文字行的大致外框：基线往下 0.25 字号、往上 0.8 字号。
fn line_box(l: &Line) -> [f64; 4] {
    [
        l.x0,
        l.y - 0.25 * l.size,
        l.x1.max(l.x0),
        l.y + 0.8 * l.size,
    ]
}

pub(super) fn center(l: &Line) -> (f64, f64) {
    ((l.x0 + l.x1) / 2.0, l.y + 0.3 * l.size)
}

pub(super) fn near(a: &[f64; 4], b: &[f64; 4], gap: f64) -> bool {
    a[0] <= b[2] + gap && b[0] <= a[2] + gap && a[1] <= b[3] + gap && b[1] <= a[3] + gap
}

pub(super) fn union(a: &mut [f64; 4], b: &[f64; 4]) {
    a[0] = a[0].min(b[0]);
    a[1] = a[1].min(b[1]);
    a[2] = a[2].max(b[2]);
    a[3] = a[3].max(b[3]);
}

/// 找一页的矢量图区域。`lines` 是这一页的行片段。
pub(crate) fn detect(
    shapes: &[Shape],
    rules: &[Rule],
    lines: &[Line],
    width: f64,
    height: f64,
) -> Vec<Figure> {
    if shapes.is_empty() && rules.is_empty() {
        return Vec::new();
    }
    let centers = Centers::new(lines);
    let index = Rules::new(rules);
    let items = evidence(shapes, rules, &index, &centers, (width, height));
    if items.is_empty() {
        return Vec::new();
    }
    // 权重要看柱的排列，算一次存起来
    let mut boxes: Vec<(u32, Cluster)> = cluster(&items, rules)
        .into_iter()
        .filter(|c| c.b[2] - c.b[0] >= MIN_W && c.b[3] - c.b[1] >= MIN_H)
        .map(|c| (c.weight(), c))
        .filter(|(w, _)| *w >= MIN_WEIGHT)
        .collect();
    // 认领顺序：权重大的先认领，同权重按位置（结果要确定）
    boxes.sort_by(|(wa, a), (wb, b)| {
        wb.cmp(wa)
            .then(b.b[3].total_cmp(&a.b[3]))
            .then(a.b[0].total_cmp(&b.b[0]))
            .then(a.b[1].total_cmp(&b.b[1]))
            .then(a.b[2].total_cmp(&b.b[2]))
    });
    if boxes.is_empty() {
        return Vec::new();
    }
    let body = body_size(lines);
    let feats = Feats::new(lines);
    let mut taken = vec![false; lines.len()];
    let mut out = Vec::new();
    for (_, Cluster { mut b, .. }) in boxes {
        extend_by_rules(&mut b, rules);
        // 和已有的图重叠：同一张图被分成了几类（流程图的几列节点），并进去
        let b4 = |f: &Figure| [f.x0, f.bottom, f.x1, f.top];
        if let Some(f) = out.iter_mut().find(|f| near(&b4(f), &b, 0.0)) {
            let mut u = b4(f);
            union(&mut u, &b);
            if let Some(g) = claim(u, lines, &feats, &mut taken, body, height, &index) {
                f.lines.extend(g.lines);
                (f.x0, f.bottom, f.x1, f.top) = (
                    f.x0.min(g.x0),
                    f.bottom.min(g.bottom),
                    f.x1.max(g.x1),
                    f.top.max(g.top),
                );
            }
            continue;
        }
        if let Some(f) = claim(b, lines, &feats, &mut taken, body, height, &index) {
            out.push(f);
        }
    }
    out
}

/// 并上贴着图的横竖线（坐标轴、网格线）：线和图框相接，长度不超过图在那个方向的 1.5 倍。
fn extend_by_rules(b: &mut [f64; 4], rules: &[Rule]) {
    let orig = *b;
    for r in rules {
        let rb = [r.x0, r.y0, r.x1, r.y1];
        if !near(&orig, &rb, 2.0) {
            continue;
        }
        let fits = if r.horizontal() {
            r.x1 - r.x0 <= 1.5 * (orig[2] - orig[0]) + 20.0
        } else {
            r.y1 - r.y0 <= 1.5 * (orig[3] - orig[1]) + 20.0
        };
        if fits {
            union(b, &rb);
        }
    }
}

/// 页上字数最多的字号。
fn body_size(lines: &[Line]) -> f64 {
    let mut hist: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for l in lines {
        *hist.entry((l.size * 2.0).round() as i64).or_default() += l.text.chars().count();
    }
    hist.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map_or(10.0, |(k, _)| k as f64 / 2.0)
}

/// 给图框认领文字：框里的行，再逐轮吸附紧贴的短标签。框里正文太多（文本框、整栏背景）就不是图。
fn claim(
    mut b: [f64; 4],
    lines: &[Line],
    feats: &Feats,
    taken: &mut [bool],
    body: f64,
    height: f64,
    index: &Rules,
) -> Option<Figure> {
    // 大字（标题、封面题名）不归图。参照字号取页面正文字号和图框里文字字号中位数的较大者：
    // 注释小字很多的页，页面“正文字号”会偏小
    let mut sizes: Vec<f64> = lines
        .iter()
        .filter(|l| {
            let (x, y) = center(l);
            x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
        })
        .map(|l| l.size)
        .collect();
    sizes.sort_by(f64::total_cmp);
    let base = sizes.get(sizes.len() / 2).map_or(body, |&m| m.max(body));
    let big = |l: &Line| (l.size > 1.3 * base && l.size - base >= 2.0) || l.size >= 15.0;
    // 写入方声明的标题（结构树的 H、对上书签的行）不归图
    let free = |i: usize, taken: &[bool]| {
        let l = &lines[i];
        !taken[i] && !l.raw && !l.artifact && !l.keep && l.hint.is_none() && !big(l)
    };
    let inside: Vec<usize> = (0..lines.len())
        .filter(|&i| free(i, taken))
        .filter(|&i| {
            let (x, y) = center(&lines[i]);
            x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
        })
        .filter(|&i| !feats.get(i).caption)
        .collect();
    let prose = inside.iter().filter(|&&i| feats.get(i).prose).count();
    if prose >= 5 || (prose >= 3 && prose * 10 >= inside.len() * 3) {
        return None;
    }
    let mut mine = inside;
    for &i in &mine {
        taken[i] = true;
        union(&mut b, &line_box(&lines[i]));
    }
    // 页眉页脚区（上下 8%）的字不吸，留给页眉页脚处理
    let margin = |l: &Line| l.y < 0.08 * height || l.y > 0.92 * height;
    for _ in 0..8 {
        let mut grew = false;
        for i in 0..lines.len() {
            if !free(i, taken) || margin(&lines[i]) {
                continue;
            }
            let l = &lines[i];
            let lb = line_box(l);
            let s = l.size;
            // 上下：横向落在图的范围内，竖向空隙不超过一个字号
            let vertical = lb[0] >= b[0] - 3.0 * s
                && lb[2] <= b[2] + 3.0 * s
                && (lb[1] - b[3]).max(b[1] - lb[3]) <= 1.0 * s;
            // 左右：竖向在图的范围内，横向空隙很小，而且是很短的标签（刻度值、单位）
            let (_, cy) = center(l);
            let side = cy >= b[1] - 0.5 * s
                && cy <= b[3] + 0.5 * s
                && (lb[0] - b[2]).max(b[0] - lb[2]) <= 1.2 * s;
            if !vertical && !side {
                continue;
            }
            // 题注、句子不吸
            let f = feats.get(i);
            if f.caption || !f.label {
                continue;
            }
            let side = side && f.short;
            // 表格格子里的字（上下都有横线）不吸：图紧挨着表格时不把表格一行行吸进来
            if (vertical || side) && !index.ruled_cell(&lb, l.size, &b) {
                taken[i] = true;
                mine.push(i);
                union(&mut b, &lb);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    if mine.is_empty() {
        return None;
    }
    Some(Figure {
        x0: b[0],
        bottom: b[1],
        x1: b[2],
        top: b[3],
        lines: mine,
    })
}

/// 图内文字合成一段：按行从上到下、行内从左到右，行间换行；行首的 `#` 转义，免得被当成标题。
pub(crate) fn text_of(lines: &[Line], members: &[usize]) -> String {
    let mut ls: Vec<&Line> = members.iter().map(|&i| &lines[i]).collect();
    ls.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x0.total_cmp(&b.x0)));
    let mut rows: Vec<Vec<&Line>> = Vec::new();
    for l in ls {
        match rows.last_mut() {
            Some(r) if (r[0].y - l.y).abs() < 0.35 * r[0].size.max(l.size) => r.push(l),
            _ => rows.push(vec![l]),
        }
    }
    let mut out = String::new();
    for mut r in rows {
        r.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let row = r
            .iter()
            .map(|l| l.text.trim())
            .collect::<Vec<_>>()
            .join(" ");
        if !out.is_empty() {
            out.push('\n');
        }
        if row.starts_with(['#', '|']) {
            out.push('\\');
        }
        out.push_str(&row);
    }
    out
}

/// 把图内的行从 `frags` 里拿出来，每个图换成一个原样输出的块；返回这些块和图外的横竖线（表格检测用）。
pub(crate) fn take(
    figs: &[Figure],
    frags: &mut Vec<Line>,
    rules: &[Rule],
) -> (Vec<Line>, Vec<Rule>) {
    if figs.is_empty() {
        return (Vec::new(), rules.to_vec());
    }
    let blocks: Vec<Line> = figs
        .iter()
        .map(|f| {
            let mut sizes: Vec<f64> = f.lines.iter().map(|&i| frags[i].size).collect();
            sizes.sort_by(f64::total_cmp);
            Line {
                text: text_of(frags, &f.lines),
                x0: f.x0,
                x1: f.x1,
                y: f.top,
                size: sizes[sizes.len() / 2],
                bold: false,
                raw: true,
                mcid: None,
                artifact: false,
                hint: None,
                group: None,
                keep: false,
            }
        })
        .collect();
    let mut gone = vec![false; frags.len()];
    for f in figs {
        for &i in &f.lines {
            gone[i] = true;
        }
    }
    let mut it = gone.iter();
    frags.retain(|_| !it.next().copied().unwrap_or(false));
    let rest = rules
        .iter()
        .copied()
        .filter(|r| {
            !figs
                .iter()
                .any(|f| f.contains((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0))
        })
        .collect();
    (blocks, rest)
}

#[cfg(test)]
mod tests;
