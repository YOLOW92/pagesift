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
    // 正文行距只拿正文行量：相邻两行字号都接近正文、而且在同一栏（水平方向有重叠）。
    // 图表里密集的坐标标签会把全页的行距中位数拉低，正文的最后一行就被当成孤立的页脚
    let span = |r: &Vec<usize>| {
        r.iter().fold((f64::MAX, f64::MIN), |(a, b), &i| {
            (a.min(lines[i].x0), b.max(lines[i].x1))
        })
    };
    let body_row = |r: &Vec<usize>| (row_size(r) - body).abs() <= body * 0.08;
    let text_gaps: Vec<f64> = rows
        .windows(2)
        .filter(|w| {
            let (a, b) = (span(&w[0]), span(&w[1]));
            body_row(&w[0]) && body_row(&w[1]) && a.0 < b.1 && b.0 < a.1
        })
        .map(|w| row_y(&w[0]) - row_y(&w[1]))
        .filter(|g| *g > 0.0)
        .collect();
    let mut gaps: Vec<f64> = if text_gaps.len() >= 3 {
        text_gaps
    } else {
        rows.windows(2)
            .map(|w| row_y(&w[0]) - row_y(&w[1]))
            .filter(|g| *g > 0.0)
            .collect()
    };
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
    // 跨页重复先算（Artifact 片段和同一行的其他文字一起才对得上别的页）
    remove_repeated_furniture(pages, heights);
    remove_fixed_furniture(pages, heights);
    for lines in pages.iter_mut() {
        remove_artifacts(lines);
    }
    for (lines, &h) in pages.iter_mut().zip(heights) {
        remove_page_furniture(lines, h);
    }
}

/// 写入方标成 `/Artifact` 的文字（ISO 32000-1 §14.8.2.2：页眉、页脚、页码、水印等，不属于正文）直接去掉。
/// 写入方的标记和版面对不上时不删：一页里大半文字都标成 Artifact（标记用错了）、
/// 字号明显比正文大（页眉页脚很少比正文大，常见的是幻灯片标题被标成页眉），以及声明过的标题。
fn remove_artifacts(lines: &mut Vec<Line>) {
    let chars = |l: &Line| l.text.chars().filter(|c| !c.is_whitespace()).count();
    let all: usize = lines.iter().map(chars).sum();
    let marked: usize = lines.iter().filter(|l| l.artifact).map(chars).sum();
    if marked == 0 || marked * 2 > all {
        return;
    }
    // 正文字号：按字数加权的中位数
    let mut sizes: Vec<(f64, usize)> = lines
        .iter()
        .filter(|l| !l.raw)
        .map(|l| (l.size, chars(l)))
        .collect();
    sizes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = sizes.iter().map(|s| s.1).sum::<usize>() / 2;
    let mut acc = 0;
    let body = sizes.iter().find(|s| {
        acc += s.1;
        acc > half
    });
    let Some(&(body, _)) = body else { return };
    lines.retain(|l| !l.artifact || l.keep || l.hint.is_some() || l.size > body * 1.15);
}

/// 跨页比较用的行键：(在页顶, 归一化文字, 字号)。
type Key = (bool, String, i64);
/// 一页的边缘行：键 + 行里的片段序号。
type EdgeRows = Vec<(Key, Vec<usize>)>;

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
    let rows_of = |lines: &[Line], h: f64| -> EdgeRows {
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
    let all_rows: Vec<EdgeRows> = pages
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
    let folios = running_folios(pages, &all_rows);
    for (p, (lines, rows)) in pages.iter_mut().zip(&all_rows).enumerate() {
        let mut drop = vec![false; lines.len()];
        for (r, (k, g)) in rows.iter().enumerate() {
            if g.iter().any(|&i| lines[i].keep || lines[i].hint.is_some()) {
                continue;
            }
            if is_page_number(&k.1)
                || counts.get(k).copied().unwrap_or(0) >= need
                || folios.contains(&(p, r))
            {
                for &i in g {
                    drop[i] = true;
                }
            }
        }
        let mut it = drop.iter();
        lines.retain(|_| !it.next().copied().unwrap_or(false));
    }
}

/// 带页码的页眉页脚（`4 Monetary Policy Report`、`Monetary Policy 41`）：页码随页序递增。
/// 去掉页码后文字相同、而且“页码 − 页序”在至少 3 页上是同一个差值的边缘行算页眉页脚。
/// 不看出现比例，所以奇偶页不同的页眉、只在一章里出现的章名页眉也能认出来。
/// 返回 (页序, 行序)。
fn running_folios(pages: &[Vec<Line>], all_rows: &[EdgeRows]) -> HashSet<(usize, usize)> {
    // (页顶, 去掉页码的文字) → [(页序, 行序, 页码 − 页序)]
    type Hits = Vec<(usize, usize, i64)>;
    let mut seen: HashMap<(bool, &str), Hits> = HashMap::new();
    for (p, rows) in all_rows.iter().enumerate() {
        for (r, (k, g)) in rows.iter().enumerate() {
            let text = k.1.as_str();
            // 只认开头或末尾是页码、并且还有别的字的行
            if text.len() < 4 || !(text.starts_with("# ") || text.ends_with(" #")) {
                continue;
            }
            let mut g = g.clone();
            g.sort_by(|&a, &b| pages[p][a].x0.total_cmp(&pages[p][b].x0));
            let raw: String = g
                .iter()
                .map(|&i| pages[p][i].text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let raw = raw.trim();
            let digits: String = if text.starts_with("# ") {
                raw.chars().take_while(char::is_ascii_digit).collect()
            } else {
                let rev: String = raw.chars().rev().take_while(char::is_ascii_digit).collect();
                rev.chars().rev().collect()
            };
            let Ok(num) = digits.parse::<i64>() else {
                continue;
            };
            seen.entry((k.0, text))
                .or_default()
                .push((p, r, num - p as i64));
        }
    }
    let mut out = HashSet::new();
    for hits in seen.values() {
        // 按不同的页数算：同一页上几行同样的文字（图表坐标轴的年份）不算
        let mut offsets: HashMap<i64, HashSet<usize>> = HashMap::new();
        for h in hits {
            offsets.entry(h.2).or_default().insert(h.0);
        }
        for h in hits {
            if offsets[&h.2].len() >= 3 {
                out.insert((h.0, h.1));
            }
        }
    }
    out
}

/// 固定位置的页眉页脚：同一行字（字号相同）在至少 3 页上出现在同一高度（相差 1pt 内），算页眉页脚。
/// 不看出现比例，所以只在一章里出现的章名页眉也能认出来；位置一点不变是比出现比例更强的证据，
/// 所以范围放宽到页面上下各 20%（年报的页眉常在离页顶 12%–16% 处）。
/// 只从页边往里剥连续的一段：遇到第一行不是固定位置重复的就停，不删到正文中间去。
fn remove_fixed_furniture(pages: &mut [Vec<Line>], heights: &[f64]) {
    const BAND: f64 = 0.20;
    // 每页边缘区里的行：(在页顶, 高度取整, 字号取整, 归一化文字) 和行里的片段序号，按离页边由近到远排
    type Row = ((bool, i64, i64, String), Vec<usize>);
    let rows_of = |lines: &[Line], h: f64| -> Vec<Row> {
        let mut idx: Vec<usize> = (0..lines.len())
            .filter(|&i| {
                !lines[i].raw && (lines[i].y >= h * (1.0 - BAND) || lines[i].y <= h * BAND)
            })
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
                let y = lines[g[0]].y;
                // 按原文比较（只合并空白、不折叠数字）：数字不同的行不是同一行，带页码的页眉由 running_folios 管
                let key = text
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                (
                    (
                        y >= h * (1.0 - BAND),
                        y.round() as i64,
                        size.round() as i64,
                        key,
                    ),
                    g,
                )
            })
            .collect()
    };
    let all: Vec<Vec<Row>> = pages
        .iter()
        .zip(heights)
        .map(|(l, &h)| rows_of(l, h))
        .collect();
    // 同一键在几页上出现；高度允许差 1pt：把 y±1 的键都算上
    let mut pages_of: HashMap<(bool, i64, i64, &str), HashSet<usize>> = HashMap::new();
    for (p, rows) in all.iter().enumerate() {
        for ((top, y, size, text), _) in rows {
            if text.is_empty() {
                continue;
            }
            for dy in -1..=1 {
                pages_of
                    .entry((*top, y + dy, *size, text.as_str()))
                    .or_default()
                    .insert(p);
            }
        }
    }
    let fixed = |k: &(bool, i64, i64, String)| {
        pages_of
            .get(&(k.0, k.1, k.2, k.3.as_str()))
            .is_some_and(|s| s.len() >= 3)
    };
    for (lines, rows) in pages.iter_mut().zip(&all) {
        let mut drop = vec![false; lines.len()];
        // 页顶从上往下、页底从下往上
        let tops = rows.iter().filter(|(k, _)| k.0);
        let bottoms = rows.iter().rev().filter(|(k, _)| !k.0);
        for side in [tops.collect::<Vec<_>>(), bottoms.collect::<Vec<_>>()] {
            for (k, g) in side {
                if !fixed(k) || g.iter().any(|&i| lines[i].keep || lines[i].hint.is_some()) {
                    break;
                }
                for &i in g {
                    drop[i] = true;
                }
            }
        }
        // 页眉页脚不会是一页的全部文字：删完不剩别的（每页只有一两行的测试页、每页同一句话）就一行不删
        if (0..lines.len()).all(|i| drop[i] || lines[i].raw) && !lines.iter().any(|l| l.raw) {
            continue;
        }
        let mut it = drop.iter();
        lines.retain(|_| !it.next().copied().unwrap_or(false));
    }
}
