//! 表格：从页面上的框线找表格，把行片段放进单元格，输出 Markdown 表格。
//!
//! - 有框线的表格：pdfplumber（MIT）的 lattice 算法移植——线段吸附、连接，求交点，从每个交点找最小的
//!   封闭矩形当单元格，共享角点的单元格归为一张表；
//! - 只有横线的三线表（学术论文常见，自研）：几条横线上下对齐地堆叠，最上和最下两条之间就是表格区域，
//!   行按基线分，列按全表文字的竖向空隙推断。
//!
//! 坐标沿用 PDF 的约定：y 向上。

use crate::interp::Rule;
use crate::layout::Line;

/// 吸附、连接、求交点的容差（页面单位，同 pdfplumber 的默认值）。
const SNAP: f64 = 3.0;
const JOIN: f64 = 3.0;
const TOL: f64 = 1.0;
/// 交点太多（复杂的图）时不找表格，免得平方级的开销。
const MAX_POINTS: usize = 3000;

#[derive(Debug, Clone)]
pub(crate) struct Table {
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    /// 顶部（y 向上，所以是较大的那个）
    pub(crate) top: f64,
    pub(crate) bottom: f64,
    pub(crate) rows: Vec<Vec<String>>,
}

impl Table {
    pub(crate) fn markdown(&self) -> String {
        let cols = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let cell = |s: &str| s.replace('|', "\\|");
        let row = |r: &Vec<String>| {
            let mut s = String::from("|");
            for i in 0..cols {
                s.push(' ');
                s.push_str(&cell(r.get(i).map(String::as_str).unwrap_or("")));
                s.push_str(" |");
            }
            s
        };
        let mut lines = vec![row(&self.rows[0]), format!("|{}", " --- |".repeat(cols))];
        lines.extend(self.rows[1..].iter().map(row));
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Copy)]
struct HEdge {
    y: f64,
    x0: f64,
    x1: f64,
}

#[derive(Debug, Clone, Copy)]
struct VEdge {
    x: f64,
    y0: f64,
    y1: f64,
}

/// 坐标相近（差不超过 `tol`）的聚成一组，每组取平均值。返回每个输入值对应的吸附后的值。
fn snap(values: &[f64], tol: f64) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut out = vec![0.0; values.len()];
    let mut group: Vec<usize> = Vec::new();
    let flush = |group: &mut Vec<usize>, out: &mut Vec<f64>| {
        if group.is_empty() {
            return;
        }
        let mean = group.iter().map(|&i| values[i]).sum::<f64>() / group.len() as f64;
        for &i in group.iter() {
            out[i] = mean;
        }
        group.clear();
    };
    for i in idx {
        if let Some(&last) = group.last()
            && values[i] - values[last] > tol
        {
            flush(&mut group, &mut out);
        }
        group.push(i);
    }
    flush(&mut group, &mut out);
    out
}

/// 吸附后，同一条直线上首尾相接（间隔不超过 JOIN）的线段连成一条。
fn merge(rules: &[Rule]) -> (Vec<HEdge>, Vec<VEdge>) {
    let hs: Vec<&Rule> = rules.iter().filter(|r| r.horizontal()).collect();
    let vs: Vec<&Rule> = rules.iter().filter(|r| !r.horizontal()).collect();
    let hy = snap(&hs.iter().map(|r| r.y0).collect::<Vec<_>>(), SNAP);
    let vx = snap(&vs.iter().map(|r| r.x0).collect::<Vec<_>>(), SNAP);
    let mut h: Vec<HEdge> = hs
        .iter()
        .zip(&hy)
        .map(|(r, &y)| HEdge {
            y,
            x0: r.x0,
            x1: r.x1,
        })
        .collect();
    let mut v: Vec<VEdge> = vs
        .iter()
        .zip(&vx)
        .map(|(r, &x)| VEdge {
            x,
            y0: r.y0,
            y1: r.y1,
        })
        .collect();
    h.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x0.total_cmp(&b.x0)));
    v.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y0.total_cmp(&b.y0)));
    let mut hj: Vec<HEdge> = Vec::new();
    for e in h {
        match hj.last_mut() {
            Some(l) if l.y == e.y && e.x0 <= l.x1 + JOIN => l.x1 = l.x1.max(e.x1),
            _ => hj.push(e),
        }
    }
    let mut vj: Vec<VEdge> = Vec::new();
    for e in v {
        match vj.last_mut() {
            Some(l) if l.x == e.x && e.y0 <= l.y1 + JOIN => l.y1 = l.y1.max(e.y1),
            _ => vj.push(e),
        }
    }
    (hj, vj)
}

/// 有框线的表格（pdfplumber 的 lattice 算法）。返回每张表的单元格（x0, top, x1, bottom）。
fn lattice(h: &[HEdge], v: &[VEdge]) -> Vec<Vec<(f64, f64, f64, f64)>> {
    // 交点：(x, y) → 经过它的竖线和横线编号
    let mut points: Vec<(f64, f64, Vec<usize>, Vec<usize>)> = Vec::new();
    for (vi, ve) in v.iter().enumerate() {
        for (hi, he) in h.iter().enumerate() {
            if ve.y0 - TOL <= he.y
                && he.y <= ve.y1 + TOL
                && he.x0 - TOL <= ve.x
                && ve.x <= he.x1 + TOL
            {
                match points.iter_mut().find(|p| p.0 == ve.x && p.1 == he.y) {
                    Some(p) => {
                        p.2.push(vi);
                        p.3.push(hi);
                    }
                    None => points.push((ve.x, he.y, vec![vi], vec![hi])),
                }
                if points.len() > MAX_POINTS {
                    return Vec::new();
                }
            }
        }
    }
    // 从左到右、从上到下
    points.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.total_cmp(&a.1)));
    let at = |x: f64, y: f64| points.iter().position(|p| p.0 == x && p.1 == y);
    let connects = |a: usize, b: usize| {
        let (p, q) = (&points[a], &points[b]);
        (p.0 == q.0 && p.2.iter().any(|e| q.2.contains(e)))
            || (p.1 == q.1 && p.3.iter().any(|e| q.3.contains(e)))
    };
    let mut cells: Vec<(f64, f64, f64, f64)> = Vec::new();
    for i in 0..points.len() {
        let (x, y) = (points[i].0, points[i].1);
        let below: Vec<usize> = (i + 1..points.len())
            .filter(|&j| points[j].0 == x && points[j].1 < y)
            .collect();
        let right: Vec<usize> = (i + 1..points.len())
            .filter(|&j| points[j].1 == y && points[j].0 > x)
            .collect();
        'found: for &b in &below {
            if !connects(i, b) {
                continue;
            }
            for &r in &right {
                if !connects(i, r) {
                    continue;
                }
                if let Some(br) = at(points[r].0, points[b].1)
                    && connects(br, r)
                    && connects(br, b)
                {
                    cells.push((x, y, points[r].0, points[b].1));
                    break 'found;
                }
            }
        }
    }
    // 共享角点的单元格归为一张表
    let corners = |c: &(f64, f64, f64, f64)| [(c.0, c.1), (c.0, c.3), (c.2, c.1), (c.2, c.3)];
    let mut tables: Vec<Vec<(f64, f64, f64, f64)>> = Vec::new();
    let mut left: Vec<(f64, f64, f64, f64)> = cells;
    while let Some(first) = left.pop() {
        let mut table = vec![first];
        let mut pts: Vec<(f64, f64)> = corners(&first).to_vec();
        loop {
            let before = table.len();
            left.retain(|c| {
                if corners(c).iter().any(|p| pts.contains(p)) {
                    table.push(*c);
                    pts.extend(corners(c));
                    false
                } else {
                    true
                }
            });
            if table.len() == before {
                break;
            }
        }
        tables.push(table);
    }
    tables
}

fn filled(t: &Table) -> usize {
    t.rows.iter().flatten().filter(|c| !c.is_empty()).count()
}

/// 片段落在哪个格子里：用片段的中心点。
fn inside(f: &Line, x0: f64, top: f64, x1: f64, bottom: f64) -> bool {
    let cx = (f.x0 + f.x1) / 2.0;
    let cy = f.y + 0.3 * f.size;
    cx >= x0 - TOL && cx <= x1 + TOL && cy <= top + TOL && cy >= bottom - TOL
}

/// 格子里的文字：按行从上到下、行内从左到右拼成一行。
fn cell_text(frags: &[&Line]) -> String {
    let mut fs: Vec<&&Line> = frags.iter().collect();
    fs.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x0.total_cmp(&b.x0)));
    let mut out = String::new();
    for f in fs {
        crate::layout::join_text(&mut out, f.text.trim());
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 把格子排成行列：按格子的边界坐标建网格，跨行跨列的格子文字放在左上角那格。
/// 返回表格和"挤"的格子占有字格子的比例（一格里有几行、或者同一行有几段：缺了框线）。
fn grid(cells: &[(f64, f64, f64, f64)], frags: &[Line], used: &mut [bool]) -> Option<(Table, f64)> {
    let mut xs: Vec<f64> = cells.iter().flat_map(|c| [c.0, c.2]).collect();
    let mut ys: Vec<f64> = cells.iter().flat_map(|c| [c.1, c.3]).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < TOL);
    ys.sort_by(|a, b| b.total_cmp(a));
    ys.dedup_by(|a, b| (*a - *b).abs() < TOL);
    let (cols, nrows) = (xs.len().checked_sub(1)?, ys.len().checked_sub(1)?);
    if cols < 2 || nrows < 2 {
        return None;
    }
    let col_of = |x: f64| xs.iter().position(|&v| (v - x).abs() < TOL);
    let row_of = |y: f64| ys.iter().position(|&v| (v - y).abs() < TOL);
    let mut rows = vec![vec![String::new(); cols]; nrows];
    let mut filled = 0;
    let mut crowded = 0;
    let mut taken = used.to_vec();
    for c in cells {
        let (Some(ci), Some(ri)) = (col_of(c.0), row_of(c.1)) else {
            continue;
        };
        if ci >= cols || ri >= nrows {
            continue;
        }
        let mine: Vec<usize> = (0..frags.len())
            .filter(|&i| !taken[i] && inside(&frags[i], c.0, c.1, c.2, c.3))
            .collect();
        let text = cell_text(&mine.iter().map(|&i| &frags[i]).collect::<Vec<_>>());
        if mine.len() >= 2 {
            crowded += 1;
        }
        for i in mine {
            taken[i] = true;
        }
        if !text.is_empty() {
            filled += 1;
        }
        rows[ri][ci] = text;
    }
    if filled < 2 {
        return None;
    }
    rows.retain(|r| r.iter().any(|c| !c.is_empty()));
    if rows.len() < 2 {
        return None;
    }
    // 确认是表格了，格子里的片段才从正文里拿掉
    used.copy_from_slice(&taken);
    let table = Table {
        x0: xs[0],
        x1: xs[cols],
        top: ys[0],
        bottom: ys[nrows],
        rows,
    };
    Some((table, crowded as f64 / filled as f64))
}

/// 三线表（自研）：横向范围相近、上下堆叠的横线。按从上到下的顺序，试相邻横线围出的各个区间
/// （从最大的开始），取能排成合格表格的最大区间，再从它后面接着找；页脚分隔线这类凑巧对齐的远处横线
/// 因此不会把正文圈进来。
fn ruled_only(h: &[HEdge], frags: &[Line], used: &mut [bool], page_width: f64) -> Vec<Table> {
    let long: Vec<&HEdge> = h
        .iter()
        .filter(|e| e.x1 - e.x0 >= 0.2 * page_width)
        .collect();
    let mut out = Vec::new();
    let mut taken = vec![false; long.len()];
    for i in 0..long.len() {
        if taken[i] {
            continue;
        }
        // 和它左右两端对齐的横线，从上到下
        let mut group: Vec<usize> = (0..long.len())
            .filter(|&j| {
                !taken[j]
                    && (long[j].x0 - long[i].x0).abs() < 6.0
                    && (long[j].x1 - long[i].x1).abs() < 6.0
            })
            .collect();
        for &j in &group {
            taken[j] = true;
        }
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|&a, &b| long[b].y.total_cmp(&long[a].y));
        let (x0, x1) = (long[i].x0, long[i].x1);
        let mut a = 0;
        while a + 1 < group.len() {
            let mut found = None;
            for b in (a + 1..group.len()).rev() {
                let (top, bottom) = (long[group[a]].y, long[group[b]].y);
                if let Some(t) = region(frags, used, x0, x1, top, bottom) {
                    found = Some((b, t));
                    break;
                }
            }
            match found {
                Some((b, (t, mine))) => {
                    for k in mine {
                        used[k] = true;
                    }
                    out.push(t);
                    a = b;
                }
                None => a += 1,
            }
        }
    }
    out
}

/// 一个候选区域里的文字能不能排成表格。成功时返回表格和用到的片段。
fn region(
    frags: &[Line],
    used: &[bool],
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
) -> Option<(Table, Vec<usize>)> {
    let mine: Vec<usize> = (0..frags.len())
        .filter(|&k| !used[k] && inside(&frags[k], x0, top, x1, bottom))
        .collect();
    if mine.len() < 4 {
        return None;
    }
    // 圈进了从左边起头的表题、表注：多半是几个表叠在一起，换小一点的区间
    let size = median_size(frags, &mine);
    let sentence = |r: &Vec<usize>| {
        let f = &frags[r[0]];
        r.len() == 1 && f.x0 - x0 < 2.0 * size && caption(&f.text)
    };
    if rows_of(frags, &mine, size).iter().any(sentence) {
        return None;
    }
    let t = text_grid(frags, &mine, x0, x1, top, bottom)?;
    Some((t, mine))
}

/// 按基线分组（从上到下）。
fn rows_of(frags: &[Line], mine: &[usize], size: f64) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = mine.to_vec();
    order.sort_by(|&a, &b| frags[b].y.total_cmp(&frags[a].y));
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for k in order {
        match rows.last_mut() {
            Some(g) if (frags[g[0]].y - frags[k].y).abs() < 0.5 * size => g.push(k),
            _ => rows.push(vec![k]),
        }
    }
    for g in &mut rows {
        g.sort_by(|&a, &b| frags[a].x0.total_cmp(&frags[b].x0));
    }
    rows
}

fn median_size(frags: &[Line], mine: &[usize]) -> f64 {
    let mut sizes: Vec<f64> = mine.iter().map(|&k| frags[k].size).collect();
    sizes.sort_by(f64::total_cmp);
    sizes.get(sizes.len() / 2).copied().unwrap_or(10.0).max(1.0)
}

/// 表题、表注的样子：Table/Fig./Note/Source/表/图/* 开头，或者八个词以上、句号结尾的整句。
/// 表格里的小节行（"Knowledge of danger signs"）和折行的续行都不是。
fn caption(text: &str) -> bool {
    let t = text.trim();
    let lower = t.to_lowercase();
    let prefix = [
        "table ", "tab. ", "figure ", "fig. ", "note", "source", "*", "†", "表", "图", "圖",
    ]
    .iter()
    .any(|p| lower.starts_with(p));
    let sentence =
        t.split_whitespace().count() >= 8 && t.ends_with('.') && !t.starts_with(char::is_lowercase);
    prefix || sentence
}

/// 页码的样子：纯数字、罗马数字、"Page 3"、"3 / 10"。
fn page_number(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    let t = t.strip_prefix("page").unwrap_or(&t).trim();
    !t.is_empty()
        && t.len() <= 9
        && (t
            .chars()
            .all(|c| c.is_ascii_digit() || c == '/' || c == ' ')
            || t.chars().all(|c| "ivxlc".contains(c)))
}

/// 编号、项目符号：左边一列全是这些的是列表，不是表格。
fn is_marker(s: &str) -> bool {
    let t = s.trim_end_matches(['.', ')', ':']);
    s.chars().count() <= 4
        && (t
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() && t.len() == 1)
            || s.chars().all(|c| "•·◦▪‣-–—*o".contains(c)))
}

/// 按基线分行、按竖向空隙分列。至少 2 行 2 列，而且每格文字不能太长（避免把分栏正文当成表格）。
/// 列的空隙只按主体行算"有几行在这里有字"：不超过五分之一的行有字就算空隙，所以跨列的表头、
/// 偶尔一格写得很长都不会把列填上。
fn text_grid(
    frags: &[Line],
    mine: &[usize],
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
) -> Option<Table> {
    let size = median_size(frags, mine);
    let row_groups = rows_of(frags, mine, size);
    if row_groups.len() < 2 {
        return None;
    }
    let width = (x1 - x0).max(1.0);
    let bins = width.ceil() as usize + 1;
    // 分列只看主体行（片段数不少于中位数的行）：表头常有跨列的格子
    let mut lens: Vec<usize> = row_groups.iter().map(Vec::len).collect();
    lens.sort_unstable();
    let need = lens[lens.len() / 2].max(2);
    let mut body: Vec<&Vec<usize>> = row_groups.iter().filter(|g| g.len() >= need).collect();
    if body.len() < 2 {
        body = row_groups.iter().collect();
    }
    let mut count = vec![0usize; bins];
    let mut mark = vec![false; bins];
    for g in body.iter().copied() {
        mark.iter_mut().for_each(|m| *m = false);
        for &k in g {
            let a = ((frags[k].x0 - x0).max(0.0) as usize).min(bins - 1);
            let b = ((frags[k].x1 - x0).max(0.0).ceil() as usize).min(bins);
            for m in &mut mark[a..b.max(a)] {
                *m = true;
            }
        }
        for (c, &m) in count.iter_mut().zip(&mark) {
            *c += m as usize;
        }
    }
    let tolerance = body.len() / 5;
    let mut cuts: Vec<f64> = Vec::new();
    let mut run = 0usize;
    for (i, &c) in count.iter().enumerate() {
        if c <= tolerance {
            run += 1;
        } else {
            if run as f64 >= 0.8 * size && i > run {
                cuts.push(x0 + (i - run / 2) as f64);
            }
            run = 0;
        }
    }
    let cols = cuts.len() + 1;
    if cols < 2 {
        return None;
    }
    let col_of = |f: &Line| cuts.iter().filter(|&&c| (f.x0 + f.x1) / 2.0 > c).count();
    let mut rows: Vec<Vec<String>> = Vec::new();
    for g in &row_groups {
        let mut cells: Vec<Vec<&Line>> = vec![Vec::new(); cols];
        for &k in g {
            cells[col_of(&frags[k])].push(&frags[k]);
        }
        rows.push(cells.iter().map(|c| cell_text(c)).collect());
    }
    let ys: Vec<f64> = row_groups.iter().map(|g| frags[g[0]].y).collect();
    let rows = merge_wrapped(rows, &ys);
    if rows.len() < 2 {
        return None;
    }
    // 正文段落不是表格：格子平均很长，或者大多数行只有一格有字
    let filled: Vec<&String> = rows.iter().flatten().filter(|c| !c.is_empty()).collect();
    let avg =
        filled.iter().map(|c| c.chars().count()).sum::<usize>() as f64 / filled.len().max(1) as f64;
    let multi = rows
        .iter()
        .filter(|r| r.iter().filter(|c| !c.is_empty()).count() >= 2)
        .count();
    if avg > 40.0 || multi * 2 < rows.len() {
        return None;
    }
    // 列表不是表格：第一列全是编号或项目符号
    let first: Vec<&String> = rows
        .iter()
        .map(|r| &r[0])
        .filter(|c| !c.is_empty())
        .collect();
    if !first.is_empty() && first.iter().all(|c| is_marker(c)) {
        return None;
    }
    let t = Table {
        x0,
        x1,
        top,
        bottom,
        rows,
    };
    (!prose(&t)).then_some(t)
}

/// 格子里折行的文字合回上一行（自研）：表格的行距有两档时，按小行距紧跟上一行、而且只在上一行
/// 有字的列里有字（至少空一格、没有数值格）的行是折行。行距只有一档时（每行都单行）不合并，所以合计行这类
/// 只有几格有字的真实行不会被并掉。
fn merge_wrapped(rows: Vec<Vec<String>>, ys: &[f64]) -> Vec<Vec<String>> {
    let gaps: Vec<f64> = ys.windows(2).map(|w| w[0] - w[1]).collect();
    let min = gaps.iter().copied().fold(f64::INFINITY, f64::min);
    if !gaps.iter().any(|&g| g >= 1.3 * min) {
        return rows;
    }
    let mut out: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        let wrapped = i > 0
            && gaps[i - 1] <= 1.15 * min
            && row.iter().any(String::is_empty)
            && !row.iter().any(|c| numeric(c))
            && out.last().is_some_and(|prev: &Vec<String>| {
                row.iter()
                    .zip(prev)
                    .all(|(c, p)| c.is_empty() || !p.is_empty())
            });
        match out.last_mut() {
            Some(prev) if wrapped => {
                for (p, c) in prev.iter_mut().zip(row) {
                    if !c.is_empty() {
                        crate::layout::join_text(p, &c);
                    }
                }
            }
            _ => out.push(row),
        }
    }
    out
}

/// 数值格（数字、金额、百分比、带括号的标准差）：数值不会折行。
fn numeric(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || " .,%$€£()+-–±:/".contains(c))
}

/// 窄栏正文排得像表格：每格是接着上一行的句子（小写开头多）、每格词数多、中日韩句中标点多；
/// 或者是大多数行带点引导线的目录。
fn prose(t: &Table) -> bool {
    let cells: Vec<&String> = t.rows.iter().flatten().filter(|c| !c.is_empty()).collect();
    let n = cells.len().max(1) as f64;
    let lower = cells
        .iter()
        .filter(|c| c.chars().next().is_some_and(char::is_lowercase))
        .count() as f64;
    let words = cells
        .iter()
        .map(|c| c.split_whitespace().count())
        .sum::<usize>() as f64;
    // 中日韩正文没有大小写和空格：看句中标点
    let cjk = cells
        .iter()
        .filter(|c| c.contains(['，', '。', '．', '、']))
        .count() as f64;
    // 目录的点引导线
    let leaders = t
        .rows
        .iter()
        .filter(|r| {
            r.iter()
                .any(|c| c.contains(". . .") || c.contains("....") || c.contains("···"))
        })
        .count();
    let leader = leaders * 2 > t.rows.len();
    lower / n > 0.25 || words / n > 4.0 || cjk / n > 0.25 || leader
}

/// 无框线的表格（自研，思路来自 Nurminen 的文本边缘检测）：一行里有两个以上短片段的是"表格行"，
/// 连续的表格行（中间允许夹一行单独的小节标题）至少三行（页眉页脚区带页码的行除外），围出候选区域，再按文字排成行列。
fn stream(frags: &[Line], used: &mut [bool], page_height: f64) -> Vec<Table> {
    let free: Vec<usize> = (0..frags.len()).filter(|&k| !used[k]).collect();
    if free.len() < 6 {
        return Vec::new();
    }
    let size = median_size(frags, &free);
    let rows = rows_of(frags, &free, size);
    // 页眉页脚区里带页码的行（"公司名    42"）不算
    let tabular = |r: &Vec<usize>| {
        if r.len() < 2 {
            return false;
        }
        let y = frags[r[0]].y;
        if (y < 0.08 * page_height || y > 0.92 * page_height)
            && r.iter().any(|&k| page_number(&frags[k].text))
        {
            return false;
        }
        let mut lens: Vec<usize> = r.iter().map(|&k| frags[k].text.chars().count()).collect();
        lens.sort_unstable();
        lens[lens.len() / 2] <= 30
    };
    let y = |r: &Vec<usize>| frags[r[0]].y;
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if !tabular(&rows[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut count = 1;
        while j < rows.len() && y(&rows[j - 1]) - y(&rows[j]) <= 3.0 * size {
            if tabular(&rows[j]) {
                count += 1;
                j += 1;
            } else if j + 1 < rows.len()
                && tabular(&rows[j + 1])
                && y(&rows[j]) - y(&rows[j + 1]) <= 3.0 * size
            {
                j += 2;
                count += 1;
            } else {
                break;
            }
        }
        if count >= 3 {
            let mine: Vec<usize> = rows[i..j].iter().flatten().copied().collect();
            let x0 = mine
                .iter()
                .map(|&k| frags[k].x0)
                .fold(f64::INFINITY, f64::min);
            let x1 = mine
                .iter()
                .map(|&k| frags[k].x1)
                .fold(f64::NEG_INFINITY, f64::max);
            let top = y(&rows[i]) + size;
            let bottom = y(&rows[j - 1]) - 0.5 * size;
            if let Some(t) = text_grid(frags, &mine, x0, x1, top, bottom) {
                for k in mine {
                    used[k] = true;
                }
                out.push(t);
            }
        }
        i = j;
    }
    out
}

/// 找一页的表格。表格里的片段从 `frags` 里拿掉，返回的表格按从上到下排列。
pub(crate) fn extract(
    rules: &[Rule],
    frags: &mut Vec<Line>,
    page_width: f64,
    page_height: f64,
) -> Vec<Table> {
    if frags.is_empty() {
        return Vec::new();
    }
    let (h, v) = merge(rules);
    let mut used = vec![false; frags.len()];
    let mut tables: Vec<Table> = Vec::new();
    if !v.is_empty() {
        for cells in lattice(&h, &v) {
            if cells.len() >= 2 {
                let mut trial = used.clone();
                if let Some((t, crowded)) = grid(&cells, frags, &mut trial) {
                    // 很多格子挤着几行几段：框线不全，按文字重排整块，格子更多就用它
                    if crowded > 0.3
                        && let Some((alt, mine)) = region(frags, &used, t.x0, t.x1, t.top, t.bottom)
                        && filled(&alt) > filled(&t)
                    {
                        for k in mine {
                            used[k] = true;
                        }
                        tables.push(alt);
                    } else {
                        used = trial;
                        tables.push(t);
                    }
                    continue;
                }
            }
            // 拼不成网格的框线（单个外框、只有部分竖线）：框住的区域按文字排排看
            let x0 = cells.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
            let x1 = cells.iter().map(|c| c.2).fold(f64::NEG_INFINITY, f64::max);
            let top = cells.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);
            let bottom = cells.iter().map(|c| c.3).fold(f64::INFINITY, f64::min);
            // 只框住两行的多半是表头底色块，交给后面整体判断
            if x1 - x0 >= 0.2 * page_width
                && let Some((t, mine)) =
                    region(frags, &used, x0, x1, top, bottom).filter(|(t, _)| t.rows.len() >= 3)
            {
                for k in mine {
                    used[k] = true;
                }
                tables.push(t);
            }
        }
    }
    // 没被有框线的表格用掉的横线，看看是不是三线表
    let free_h: Vec<HEdge> = h
        .iter()
        .copied()
        .filter(|e| {
            !tables.iter().any(|t| {
                e.y <= t.top + SNAP
                    && e.y >= t.bottom - SNAP
                    && e.x0 >= t.x0 - SNAP
                    && e.x1 <= t.x1 + SNAP
            })
        })
        .collect();
    tables.extend(ruled_only(&free_h, frags, &mut used, page_width));
    tables.extend(stream(frags, &mut used, page_height));
    let mut it = used.iter();
    frags.retain(|_| !it.next().copied().unwrap_or(false));
    tables.sort_by(|a, b| b.top.total_cmp(&a.top));
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(text: &str, x0: f64, y: f64) -> Line {
        let x1 = x0 + 5.0 * text.chars().count() as f64;
        Line {
            text: text.into(),
            x0,
            x1,
            y,
            size: 10.0,
            bold: false,
            raw: false,
        }
    }

    fn hrule(x0: f64, x1: f64, y: f64) -> Rule {
        Rule {
            x0,
            y0: y,
            x1,
            y1: y,
        }
    }

    fn vrule(x: f64, y0: f64, y1: f64) -> Rule {
        Rule {
            x0: x,
            y0,
            x1: x,
            y1,
        }
    }

    #[test]
    fn lattice_grid() {
        let rules = vec![
            hrule(100.0, 300.0, 700.0),
            hrule(100.0, 300.0, 680.0),
            hrule(100.0, 300.0, 660.0),
            vrule(100.0, 660.0, 700.0),
            vrule(200.0, 660.0, 700.0),
            vrule(300.0, 660.0, 700.0),
        ];
        let mut frags = vec![
            frag("Name", 105.0, 686.0),
            frag("Age", 205.0, 686.0),
            frag("Ann", 105.0, 666.0),
            frag("42", 205.0, 666.0),
            frag("body text", 100.0, 500.0),
        ];
        let tables = extract(&rules, &mut frags, 600.0, 800.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows, [["Name", "Age"], ["Ann", "42"]]);
        assert_eq!(frags.len(), 1, "table fragments leave the body text");
    }

    #[test]
    fn booktabs_with_wrapped_cells() {
        // 三线表：顶线、表头下的线、底线；第二行的第一格折成两行
        let rules = vec![
            hrule(100.0, 400.0, 720.0),
            hrule(100.0, 400.0, 700.0),
            hrule(100.0, 400.0, 630.0),
        ];
        let mut frags = vec![
            frag("Step", 100.0, 706.0),
            frag("Date", 250.0, 706.0),
            frag("Begin", 100.0, 686.0),
            frag("May 2", 250.0, 686.0),
            frag("Complete the", 100.0, 666.0),
            frag("June 9", 250.0, 666.0),
            frag("design", 100.0, 654.0),
            frag("Ship", 100.0, 634.0),
            frag("July 1", 250.0, 634.0),
        ];
        let tables = extract(&rules, &mut frags, 600.0, 800.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables[0].rows,
            [
                ["Step", "Date"],
                ["Begin", "May 2"],
                ["Complete the design", "June 9"],
                ["Ship", "July 1"]
            ]
        );
    }

    #[test]
    fn borderless_table() {
        let mut frags = vec![frag(
            "Revenue grew this quarter thanks to strong sales in all regions.",
            50.0,
            700.0,
        )];
        for (i, (k, a, b)) in [
            ("Q1", "$448", "5%"),
            ("Q2", "$427", "7%"),
            ("Q3", "$512", "3%"),
            ("Q4", "$530", "4%"),
        ]
        .iter()
        .enumerate()
        {
            let y = 650.0 - 15.0 * i as f64;
            frags.extend([frag(k, 50.0, y), frag(a, 150.0, y), frag(b, 250.0, y)]);
        }
        let tables = extract(&[], &mut frags, 600.0, 800.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[0], ["Q1", "$448", "5%"]);
        assert_eq!(tables[0].rows.len(), 4);
        assert_eq!(frags.len(), 1);
    }

    #[test]
    fn narrow_columns_of_prose_are_not_a_table() {
        let left = [
            "the committee met in",
            "spring and agreed to",
            "extend the program for",
            "another two years after",
        ];
        let right = [
            "long debate. members said",
            "the results had been",
            "better than expected and",
            "funding would continue",
        ];
        let mut frags = Vec::new();
        for i in 0..4 {
            let y = 700.0 - 12.0 * i as f64;
            frags.push(frag(left[i], 50.0, y));
            frags.push(frag(right[i], 300.0, y));
        }
        assert!(extract(&[], &mut frags, 600.0, 800.0).is_empty());
        assert_eq!(frags.len(), 8);
    }

    #[test]
    fn numbered_list_is_not_a_table() {
        let mut frags = Vec::new();
        for (i, t) in [
            "Open the box",
            "Remove the cover",
            "Plug it in",
            "Press start",
        ]
        .iter()
        .enumerate()
        {
            let y = 700.0 - 14.0 * i as f64;
            frags.push(frag(&format!("{}.", i + 1), 50.0, y));
            frags.push(frag(t, 80.0, y));
        }
        assert!(extract(&[], &mut frags, 600.0, 800.0).is_empty());
    }

    #[test]
    fn footer_with_page_number_is_not_a_table_row() {
        assert!(
            page_number("42")
                && page_number("Page 3")
                && page_number("xii")
                && page_number("3 / 10")
        );
        assert!(!page_number("Q3") && !page_number("$448"));
    }
}
