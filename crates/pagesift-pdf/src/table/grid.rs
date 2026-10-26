//! 表格网格与单元格文字分配、对齐与折行合并。

use super::Table;
use super::lattice::TOL;
use crate::layout::Line;

/// 判断文本片段是否落在矩形单元格内（基于片段中心点判定）。
pub(crate) fn inside(f: &Line, x0: f64, top: f64, x1: f64, bottom: f64) -> bool {
    let cx = (f.x0 + f.x1) / 2.0;
    let cy = f.y + 0.3 * f.size;
    cx >= x0 - TOL && cx <= x1 + TOL && cy <= top + TOL && cy >= bottom - TOL
}

/// 提取格子内的文字：按行从上到下、行内从左到右拼合。
pub(crate) fn cell_text(frags: &[&Line]) -> String {
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
pub(crate) fn grid(
    cells: &[(f64, f64, f64, f64)],
    frags: &[Line],
    used: &mut [bool],
) -> Option<(Table, f64, f64)> {
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
    let mut horiz_crowded = 0;
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
        let mut fs: Vec<&Line> = mine.iter().map(|&i| &frags[i]).collect();
        fs.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x0.total_cmp(&b.x0)));
        let text = cell_text(&fs);
        if fs.windows(2).any(|w| {
            (w[0].y - w[1].y).abs() < 0.4 * w[0].size.max(w[1].size)
                && (w[1].x0 - w[0].x1).abs() > 2.0 * w[0].size.max(w[1].size)
        }) {
            horiz_crowded += 1;
        }
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
    used.copy_from_slice(&taken);
    let table = Table {
        x0: xs[0],
        x1: xs[cols],
        top: ys[0],
        bottom: ys[nrows],
        rows,
    };
    Some((
        table,
        crowded as f64 / filled as f64,
        horiz_crowded as f64 / filled as f64,
    ))
}

/// 一个候选区域里的文字能不能排成表格。成功时返回表格和用到的片段。
pub(crate) fn region(
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
    let size = median_size(frags, &mine);
    let sentence = |r: &Vec<usize>| {
        let f = &frags[r[0]];
        r.len() == 1 && f.x0 - x0 < 2.0 * size && super::borderless::caption(&f.text)
    };
    if rows_of(frags, &mine, size).iter().any(sentence) {
        return None;
    }
    let t = text_grid(frags, &mine, x0, x1, top, bottom)?;
    Some((t, mine))
}

/// 按基线分组（从上到下）。
pub(crate) fn rows_of(frags: &[Line], mine: &[usize], size: f64) -> Vec<Vec<usize>> {
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

pub(crate) fn median_size(frags: &[Line], mine: &[usize]) -> f64 {
    let mut sizes: Vec<f64> = mine.iter().map(|&k| frags[k].size).collect();
    sizes.sort_by(f64::total_cmp);
    sizes.get(sizes.len() / 2).copied().unwrap_or(10.0).max(1.0)
}

/// 按基线分行、按竖向空隙分列。自适应空隙与跨列表头容忍。
/// 列直方图的桶数上限，见 [`text_grid`]。
const MAX_BINS: usize = 16_384;

pub(crate) fn text_grid(
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
    // 每个桶 1 点宽；损坏或恶意文件的坐标可以大到 1e300，桶数和每行的扫描都跟着失控
    // （模糊测试找到：15 万字节的文件卡 20 秒）。超过上限时按比例放宽每个桶，
    // 正常页面（最大 14400 点）比例是 1，结果不变。
    let scale = (width / MAX_BINS as f64).max(1.0);
    let bins = (width / scale).ceil() as usize + 1;
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
            let a = (((frags[k].x0 - x0) / scale).max(0.0) as usize).min(bins - 1);
            let b = (((frags[k].x1 - x0) / scale).max(0.0).ceil() as usize).min(bins);
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
            if run as f64 * scale >= 0.8 * size && i > run {
                cuts.push(x0 + (i - run / 2) as f64 * scale);
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
    let first: Vec<&String> = rows
        .iter()
        .map(|r| &r[0])
        .filter(|c| !c.is_empty())
        .collect();
    if !first.is_empty() && first.iter().all(|c| super::borderless::is_marker(c)) {
        return None;
    }
    let t = Table {
        x0,
        x1,
        top,
        bottom,
        rows,
    };
    (!super::borderless::prose(&t)).then_some(t)
}

/// 格子里折行的文字合回上一行。
pub(crate) fn merge_wrapped(rows: Vec<Vec<String>>, ys: &[f64]) -> Vec<Vec<String>> {
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

pub(crate) fn numeric(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || " .,%$€£()+-–±:/".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(text: &str, x0: f64, y: f64) -> Line {
        Line {
            text: text.into(),
            x0,
            x1: x0 + 10.0,
            y,
            size: 10.0,
            bold: false,
            raw: false,
            mcid: None,
            artifact: false,
            hint: None,
            group: None,
            keep: false,
        }
    }

    /// 模糊测试找到的：坐标大到上千万的文字让列直方图的桶数和每行的扫描失控（20 秒）。
    #[test]
    fn huge_region_width_is_bounded() {
        let mut frags = Vec::new();
        for row in 0..300 {
            let y = 700.0 - row as f64 * 12.0;
            frags.push(frag("a", 0.0, y));
            frags.push(frag("b", 100.0, y));
            frags.push(frag("c", 2.0e7, y));
        }
        let mine: Vec<usize> = (0..frags.len()).collect();
        let t = std::time::Instant::now();
        let table = text_grid(&frags, &mine, 0.0, 2.0e7 + 10.0, 700.0, -3000.0);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            t.elapsed()
        );
        std::hint::black_box(table);
    }
}
