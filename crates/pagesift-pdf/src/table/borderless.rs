//! 无框线表格与三线表（Booktabs）检测、候选区域判定与非表格内容过滤。

use super::Table;
use super::grid::{median_size, region, rows_of};
use super::lattice::HEdge;
use crate::layout::Line;

/// 表题、表注的特征判定。
pub(crate) fn caption(text: &str) -> bool {
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
pub(crate) fn page_number(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    let t = t.strip_prefix("page").unwrap_or(&t).trim();
    !t.is_empty()
        && t.len() <= 9
        && (t
            .chars()
            .all(|c| c.is_ascii_digit() || c == '/' || c == ' ')
            || t.chars().all(|c| "ivxlc".contains(c)))
}

/// 编号、项目符号判定。
pub(crate) fn is_marker(s: &str) -> bool {
    let t = s.trim_end_matches(['.', ')', ':']);
    s.chars().count() <= 4
        && (t
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() && t.len() == 1)
            || s.chars().all(|c| "•·◦▪‣-–—*o".contains(c)))
}

/// 窄栏正文过滤：检查是否误把正文或带引导线的目录识别为表格。
pub(crate) fn prose(t: &Table) -> bool {
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
    let cjk = cells
        .iter()
        .filter(|c| c.contains(['，', '。', '．', '、']))
        .count() as f64;
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

/// 三线表检测（横向范围相近、上下对齐的横线组合）。
pub(crate) fn ruled_only(
    h: &[HEdge],
    frags: &[Line],
    used: &mut [bool],
    page_width: f64,
) -> Vec<Table> {
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

/// 无框线表格检测（基于多列短片段连通流分析）。
/// `accept` 再核对一遍候选的片段（分栏之后的判断）。
pub(crate) fn stream(
    frags: &[Line],
    used: &mut [bool],
    page_height: f64,
    accept: &mut dyn FnMut(&[usize]) -> bool,
) -> Vec<Table> {
    let free: Vec<usize> = (0..frags.len()).filter(|&k| !used[k]).collect();
    if free.len() < 6 {
        return Vec::new();
    }
    let size = median_size(frags, &free);
    let rows = rows_of(frags, &free, size);
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
        lens[lens.len() / 2] <= 32
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
        // 允许自适应最大行距达 3.5 * size，防止跨段小节断裂
        while j < rows.len() && y(&rows[j - 1]) - y(&rows[j]) <= 3.5 * size {
            if tabular(&rows[j]) {
                count += 1;
                j += 1;
            } else if j + 1 < rows.len()
                && tabular(&rows[j + 1])
                && y(&rows[j]) - y(&rows[j + 1]) <= 3.5 * size
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
            if let Some(t) =
                super::grid::text_grid(frags, &mine, x0, x1, top, bottom).filter(|_| accept(&mine))
            {
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
