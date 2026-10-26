use std::collections::{HashMap, HashSet};

use super::Line;

/// 页眉页脚判定用的归一化：数字串折叠成 #，空白合并。
pub(super) fn furniture_key(text: &str) -> String {
    let mut out = String::new();
    let mut in_digits = false;
    for c in text.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
            continue;
        }
        in_digits = false;
        if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    let out = out.trim().to_string();
    let roman = !out.is_empty()
        && out.len() <= 6
        && out.chars().all(|c| matches!(c, 'i' | 'v' | 'x' | 'l'));
    if roman { "#".to_string() } else { out }
}

pub(super) fn is_page_number(key: &str) -> bool {
    let k: String = key
        .chars()
        .filter(|c| !matches!(c, '-' | '–' | '—' | ' ' | '|' | '[' | ']' | '(' | ')'))
        .collect();
    matches!(
        k.as_str(),
        "#" | "page#" | "p.#" | "#/#" | "page#of#" | "#of#" | "page#/#" | "第#页" | "第#頁"
    )
}

/// 页眉页脚里常见的内容：网址、DOI、卷期号、版权、"et al."、页码说法。
fn looks_like_furniture(text: &str) -> bool {
    let t = text.to_lowercase();
    let has = |p: &str| t.contains(p);
    has("http://")
        || has("https://")
        || has("www.")
        || has("doi")
        || has("issn")
        || has("©")
        || has("copyright")
        || has("et al.")
        || has("vol.")
        || has("vol ")
        || has("no.")
        || has("pp.")
        || has("page ")
        || has("journal")
        || has("proceedings")
        || (t.chars().any(|c| c.is_ascii_digit())
            && (has(":") || has("("))
            && t.chars().filter(|c| c.is_ascii_digit()).count() >= 4)
}

/// 单页规则（自研，单页文档也适用）：页面最上、最下的边缘区里，和正文隔得明显比行距远的行
/// （或者字号更小、又有一定间隔的行，或者像页码、网址、刊名卷期的行）是页眉页脚。
/// 字号明显比正文大的不删（多半是标题），和下一行隔着正常行距的也不删（正文第一行）。
fn remove_page_furniture(lines: &mut Vec<Line>, h: f64) {
    if lines.len() < 4 {
        return;
    }
    // 先按基线聚成行（表格不参与）
    let mut idx: Vec<usize> = (0..lines.len()).filter(|&i| !lines[i].raw).collect();
    idx.sort_by(|&a, &b| lines[b].y.total_cmp(&lines[a].y));
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in idx {
        match rows.last_mut() {
            Some(r)
                if (lines[r[0]].y - lines[i].y).abs()
                    < 0.35 * lines[r[0]].size.max(lines[i].size) =>
            {
                r.push(i)
            }
            _ => rows.push(vec![i]),
        }
    }
    if rows.len() < 4 {
        return;
    }
    let row_y = |r: &Vec<usize>| lines[r[0]].y;
    let row_size = |r: &Vec<usize>| r.iter().map(|&i| lines[i].size).fold(0.0, f64::max);
    let row_text = |r: &Vec<usize>| {
        let mut r = r.clone();
        r.sort_by(|&a, &b| lines[a].x0.total_cmp(&lines[b].x0));
        r.iter()
            .map(|&i| lines[i].text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut sizes: Vec<f64> = rows.iter().map(row_size).collect();
    sizes.sort_by(f64::total_cmp);
    let body = sizes[sizes.len() / 2];
    let mut gaps: Vec<f64> = rows
        .windows(2)
        .map(|w| row_y(&w[0]) - row_y(&w[1]))
        .filter(|g| *g > 0.0)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let line_gap = gaps
        .get(gaps.len() / 2)
        .copied()
        .unwrap_or(body * 1.2)
        .max(body);
    let mut drop = vec![false; lines.len()];
    // edge: 0 从顶部往下，1 从底部往上
    for edge in 0..2 {
        let order: Vec<usize> = if edge == 0 {
            (0..rows.len()).collect()
        } else {
            (0..rows.len()).rev().collect()
        };
        for (k, &ri) in order.iter().enumerate() {
            let r = &rows[ri];
            let y = row_y(r);
            let in_zone = if edge == 0 {
                y >= h * 0.88
            } else {
                y <= h * 0.12
            };
            if !in_zone || k + 1 >= order.len() {
                break;
            }
            let next = &rows[order[k + 1]];
            let gap = (y - row_y(next)).abs();
            let size = row_size(r);
            let text = row_text(r);
            if size > body * 1.15 {
                break;
            }
            // 页眉页脚很少是带章节编号的标题或粗体行；只出现在这一页的这种行多半是页首的小节标题
            // （跨页重复的页眉已经由重复规则去掉）
            // 写入方声明过的标题（对上书签、结构树标成标题）不删
            if r.iter().any(|&i| lines[i].keep || lines[i].hint.is_some()) {
                break;
            }
            let isolated = gap >= 1.6 * line_gap;
            let small = size <= body * 0.92 && gap >= 1.25 * line_gap;
            let pattern = (is_page_number(&furniture_key(&text)) || looks_like_furniture(&text))
                && gap >= 1.1 * line_gap;
            if isolated || small || pattern {
                for &i in r {
                    drop[i] = true;
                }
            } else {
                break;
            }
        }
    }
    let mut it = drop.iter();
    lines.retain(|_| !it.next().copied().unwrap_or(false));
}

/// 去掉每页重复出现在页面顶部或底部的片段（页眉、页脚、页码），再按单页规则去掉页面边缘的页眉页脚。
pub(crate) fn remove_furniture(pages: &mut [Vec<Line>], heights: &[f64]) {
    remove_repeated_furniture(pages, heights);
    for (lines, &h) in pages.iter_mut().zip(heights) {
        remove_page_furniture(lines, h);
    }
}

fn remove_repeated_furniture(pages: &mut [Vec<Line>], heights: &[f64]) {
    let n = pages.len();
    if n < 2 {
        return;
    }
    let zone = |line: &Line, h: f64| -> Option<bool> {
        if line.y >= h * 0.88 {
            Some(true)
        } else if line.y <= h * 0.12 {
            Some(false)
        } else {
            None
        }
    };
    // 按整行判定：两端对齐会把一行切成很多片段，单个词不能拿来比较。
    // 字号也算进键里：首页的大标题和每页页眉里的同名小字分开计数。
    type Key = (bool, String, i64);
    let rows_of = |lines: &[Line], h: f64| -> Vec<(Key, Vec<usize>)> {
        let mut idx: Vec<usize> = (0..lines.len())
            .filter(|&i| !lines[i].raw && zone(&lines[i], h).is_some())
            .collect();
        idx.sort_by(|&a, &b| lines[b].y.total_cmp(&lines[a].y));
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for i in idx {
            match groups.last_mut() {
                Some(g)
                    if (lines[g[0]].y - lines[i].y).abs()
                        < 0.35 * lines[g[0]].size.max(lines[i].size) =>
                {
                    g.push(i)
                }
                _ => groups.push(vec![i]),
            }
        }
        groups
            .into_iter()
            .map(|mut g| {
                g.sort_by(|&a, &b| lines[a].x0.total_cmp(&lines[b].x0));
                let text: Vec<&str> = g.iter().map(|&i| lines[i].text.as_str()).collect();
                let size = g.iter().map(|&i| lines[i].size).fold(0.0, f64::max);
                let top = zone(&lines[g[0]], h) == Some(true);
                (
                    (top, furniture_key(&text.join(" ")), size.round() as i64),
                    g,
                )
            })
            .collect()
    };
    let all_rows: Vec<Vec<(Key, Vec<usize>)>> = pages
        .iter()
        .zip(heights)
        .map(|(lines, &h)| rows_of(lines, h))
        .collect();
    let mut counts: HashMap<&Key, usize> = HashMap::new();
    for rows in &all_rows {
        let seen: HashSet<&Key> = rows.iter().map(|(k, _)| k).collect();
        for k in seen {
            *counts.entry(k).or_default() += 1;
        }
    }
    let need = (n * 2).div_ceil(5).max(2);
    for (lines, rows) in pages.iter_mut().zip(&all_rows) {
        let mut drop = vec![false; lines.len()];
        for (k, g) in rows {
            if g.iter().any(|&i| lines[i].keep || lines[i].hint.is_some()) {
                continue;
            }
            if is_page_number(&k.1) || counts.get(k).copied().unwrap_or(0) >= need {
                for &i in g {
                    drop[i] = true;
                }
            }
        }
        let mut it = drop.iter();
        lines.retain(|_| !it.next().copied().unwrap_or(false));
    }
}
