//! 版面重组：字形 → 行片段 → 分栏阅读顺序 → 段落 → Markdown。

use std::collections::{HashMap, HashSet};

use crate::interp::Glyph;

/// 同一基线上连续的一段文字。分栏以前叫"片段"，合并同基线片段后就是"行"。
#[derive(Debug, Clone)]
pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    pub(crate) y: f64,
    pub(crate) size: f64,
    pub(crate) bold: bool,
    /// 原样输出的内容（表格的 Markdown），单独成段，不参与合并。
    pub(crate) raw: bool,
}

/// 片段内超过这个间距（相对字号）就拆开，拆多了没关系，分栏后会按基线合回去。
const SPLIT_GAP: f64 = 0.8;
const SPACE_GAP: f64 = 0.15;
const CJK_SPACE_GAP: f64 = 0.5;

pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2FDF | 0x3000..=0x30FF | 0x3100..=0x312F | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF
        | 0x20000..=0x2FFFF)
}

fn normalize_char(c: char, out: &mut String) {
    match c {
        '\u{FB00}' => out.push_str("ff"),
        '\u{FB01}' => out.push_str("fi"),
        '\u{FB02}' => out.push_str("fl"),
        '\u{FB03}' => out.push_str("ffi"),
        '\u{FB04}' => out.push_str("ffl"),
        '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{3000}' | '\t' => out.push(' '),
        '\u{200B}'..='\u{200D}' | '\u{FEFF}' | '\u{FFFD}' => {}
        c if c.is_control() => {}
        c => out.push(crate::encoding::unify_radical(c)),
    }
}

/// TeX 用两个字形拼出来的符号合成一个：否定斜杠叠在后一个符号上（̸= → ≠），`\mapsto` 是短竖加箭头。
fn compose_tex(text: &str) -> String {
    if !text.contains(['\u{0338}', '↦']) {
        return text.to_string();
    }
    let negated = |c: char| match c {
        '=' => Some('≠'),
        '∈' => Some('∉'),
        '∋' => Some('∌'),
        '<' => Some('≮'),
        '>' => Some('≯'),
        '≤' => Some('≰'),
        '≥' => Some('≱'),
        '⊂' => Some('⊄'),
        '⊃' => Some('⊅'),
        '⊆' => Some('⊈'),
        '⊇' => Some('⊉'),
        '∼' => Some('≁'),
        '≃' => Some('≄'),
        '≈' => Some('≉'),
        '≡' => Some('≢'),
        '∃' => Some('∄'),
        '|' | '∣' => Some('∤'),
        '∥' => Some('∦'),
        _ => None,
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\u{0338}'
            && let Some(n) = next.and_then(negated)
        {
            out.push(n);
            i += 2;
            continue;
        }
        if c == '↦' && next == Some('→') {
            out.push('↦');
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 把一页的字形聚合成行片段。
pub(crate) fn fragments(glyphs: &[Glyph]) -> Vec<Line> {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by(|&a, &b| {
        glyphs[b]
            .y
            .total_cmp(&glyphs[a].y)
            .then(glyphs[a].x.total_cmp(&glyphs[b].x))
    });

    // 先严格按基线聚成行，再把上下标这种小字号短行挂到相邻的大字号行上。
    // 不能用宽松容差一次聚完：大号标题会把旁边一栏的上下两行串在一起。
    struct Row {
        idx: Vec<usize>,
        y: f64,
        size: f64,
        x0: f64,
        x1: f64,
    }
    let mut rows: Vec<Row> = Vec::new();
    for i in order {
        let g = &glyphs[i];
        match rows.last_mut() {
            Some(r) if (r.y - g.y).abs() <= 0.15 * r.size.max(g.size) => {
                r.idx.push(i);
                r.size = r.size.max(g.size);
                r.x0 = r.x0.min(g.x);
                r.x1 = r.x1.max(g.x_end);
            }
            _ => rows.push(Row {
                idx: vec![i],
                y: g.y,
                size: g.size,
                x0: g.x,
                x1: g.x_end.max(g.x),
            }),
        }
    }
    for i in 0..rows.len() {
        let r = &rows[i];
        if r.idx.is_empty() {
            continue;
        }
        let mut best: Option<(usize, f64)> = None;
        for (j, h) in rows
            .iter()
            .enumerate()
            .take((i + 9).min(rows.len()))
            .skip(i.saturating_sub(8))
        {
            let dy = (h.y - r.y).abs();
            if j != i
                && h.size >= r.size * 1.1
                && h.idx.len() >= r.idx.len() * 2
                && dy < 0.6 * h.size
                && r.x0 <= h.x1 + 0.5 * h.size
                && r.x1 >= h.x0 - 0.5 * h.size
                && best.is_none_or(|(_, d)| dy < d)
            {
                best = Some((j, dy));
            }
        }
        if let Some((j, _)) = best {
            let moved = std::mem::take(&mut rows[i].idx);
            rows[j].idx.extend(moved);
        }
    }
    let clusters: Vec<Vec<usize>> = rows
        .into_iter()
        .map(|r| r.idx)
        .filter(|c| !c.is_empty())
        .collect();

    let mut out = Vec::new();
    for mut c in clusters {
        c.sort_by(|&a, &b| glyphs[a].x.total_cmp(&glyphs[b].x));
        let mut cur: Vec<&Glyph> = Vec::new();
        let mut prev_end = f64::NEG_INFINITY;
        for &i in &c {
            let g = &glyphs[i];
            if let Some(p) = cur.last() {
                // 重复叠印（假粗体）只留一份
                if p.text == g.text
                    && (p.x - g.x).abs() < 0.2 * g.size
                    && (p.y - g.y).abs() < 0.2 * g.size
                {
                    continue;
                }
                let gap = g.x - prev_end;
                if gap > SPLIT_GAP * g.size.max(p.size) {
                    out.extend(make_line(&cur));
                    cur.clear();
                    prev_end = f64::NEG_INFINITY;
                }
            }
            prev_end = prev_end.max(g.x_end).max(g.x);
            cur.push(g);
        }
        out.extend(make_line(&cur));
    }
    out
}

fn make_line(glyphs: &[&Glyph]) -> Option<Line> {
    // 按空格切词；词内文字和原来逐字拼接一致
    let mut words: Vec<Vec<crate::math::Item>> = Vec::new();
    let mut pending_space = false;
    let mut prev: Option<&Glyph> = None;
    let mut prev_end = f64::NEG_INFINITY;
    let mut bold_chars = 0usize;
    let mut all_chars = 0usize;
    let mut sizes: HashMap<i64, (usize, f64, f64)> = HashMap::new();
    let mut last_char: Option<char> = None;
    for g in glyphs {
        let mut t = String::new();
        for c in g.text.chars() {
            normalize_char(c, &mut t);
        }
        if t.trim().is_empty() {
            if !t.is_empty() {
                pending_space = true;
            }
            continue;
        }
        let mut new_word = words.is_empty();
        if let Some(p) = prev {
            let gap = g.x - prev_end;
            let cjk = last_char.is_some_and(is_cjk) && t.chars().next().is_some_and(is_cjk);
            let limit = if cjk { CJK_SPACE_GAP } else { SPACE_GAP };
            if pending_space || gap > limit * g.size.max(p.size) {
                new_word = true;
            }
        }
        pending_space = false;
        let n = t.chars().count();
        all_chars += n;
        if g.bold {
            bold_chars += n;
        }
        let key = (g.size * 10.0).round() as i64;
        let e = sizes.entry(key).or_insert((0, g.size, g.y));
        e.0 += n;
        last_char = t.chars().last().or(last_char);
        let item = crate::math::Item {
            g,
            text: t.trim_start().to_string(),
        };
        match words.last_mut() {
            Some(w) if !new_word => w.push(item),
            _ => words.push(vec![item]),
        }
        prev_end = prev_end.max(g.x_end).max(g.x);
        prev = Some(g);
    }
    // 字号取字符数最多的那个，基线也跟着它
    let (_, size, y) = sizes
        .into_values()
        .max_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)))?;
    let text = crate::math::line_text(&words, y, size).unwrap_or_else(|| {
        let mut text = String::new();
        for w in &words {
            if !text.is_empty() && !text.ends_with(' ') {
                text.push(' ');
            }
            for i in w {
                text.push_str(&i.text);
            }
        }
        text
    });
    let text = compose_tex(text.trim_end());
    if text.is_empty() {
        return None;
    }
    let first = glyphs.iter().find(|g| !g.text.trim().is_empty())?;
    let last_end = glyphs
        .iter()
        .map(|g| g.x_end.max(g.x))
        .fold(f64::NEG_INFINITY, f64::max);
    Some(Line {
        text,
        x0: first.x,
        x1: last_end,
        y,
        size,
        bold: bold_chars * 10 >= all_chars * 7,
        raw: false,
    })
}

/// 页眉页脚判定用的归一化：数字串折叠成 #，空白合并。
fn furniture_key(text: &str) -> String {
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

fn is_page_number(key: &str) -> bool {
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

/// 找分栏的栏间空白，返回每条空白的 [左, 右]。
fn gutters(frags: &[Line]) -> Vec<(f64, f64)> {
    let left = frags.iter().map(|f| f.x0).fold(f64::INFINITY, f64::min);
    let right = frags.iter().map(|f| f.x1).fold(f64::NEG_INFINITY, f64::max);
    let width = right - left;
    if width.is_nan() || width <= 1.0 || frags.len() < 6 {
        return Vec::new();
    }
    let cands: Vec<&Line> = frags
        .iter()
        .filter(|f| f.x1 - f.x0 < 0.55 * width)
        .collect();
    if cands.len() < 6 {
        return Vec::new();
    }
    let mut sizes: Vec<f64> = cands.iter().map(|f| f.size).collect();
    sizes.sort_by(f64::total_cmp);
    let med = sizes[sizes.len() / 2];
    let min_gap = (0.8 * med).max(4.0);

    // 1pt 精度的覆盖直方图，宽页面按比例缩放
    let bins = (width.ceil() as usize).clamp(1, 4000);
    let scale = bins as f64 / width;
    let mut cover = vec![0usize; bins + 1];
    for f in &cands {
        let a = (((f.x0 - left) * scale).floor().max(0.0) as usize).min(bins);
        let b = (((f.x1 - left) * scale).ceil().max(0.0) as usize).min(bins);
        for c in &mut cover[a..b.max(a)] {
            *c += 1;
        }
    }
    let tol = cands.len() / 20;
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &c) in cover.iter().enumerate() {
        let empty = i < bins && c <= tol;
        match (empty, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                start = None;
                let (g0, g1) = (left + s as f64 / scale, left + i as f64 / scale);
                if s == 0 || i >= bins || g1 - g0 < min_gap {
                    continue;
                }
                let lhs = cands.iter().filter(|f| f.x1 <= g0 + 0.5).count();
                let rhs = cands.iter().filter(|f| f.x0 >= g1 - 0.5).count();
                let need = (cands.len() / 10).max(3);
                if lhs >= need && rhs >= need {
                    out.push((g0, g1));
                }
            }
            _ => {}
        }
    }
    out
}

/// 按阅读顺序把片段分成块：每块是同一栏里连续的行，跨栏的内容单独成块。
pub(crate) fn blocks(mut frags: Vec<Line>) -> Vec<Vec<Line>> {
    if frags.is_empty() {
        return Vec::new();
    }
    let gs = gutters(&frags);
    frags.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x0.total_cmp(&b.x0)));
    let column_of = |f: &Line| -> Option<usize> {
        let mut col = 0;
        for (i, &(g0, g1)) in gs.iter().enumerate() {
            let mid = (g0 + g1) / 2.0;
            if f.x0 < mid && f.x1 > mid {
                return None;
            }
            if f.x0 >= mid {
                col = i + 1;
            }
        }
        Some(col)
    };

    let mut out = Vec::new();
    let mut band: Vec<Vec<Line>> = vec![Vec::new(); gs.len() + 1];
    let mut spanning: Vec<Line> = Vec::new();
    for f in frags {
        match column_of(&f) {
            Some(c) => {
                if !spanning.is_empty() {
                    out.push(merge_rows(std::mem::take(&mut spanning)));
                }
                band[c].push(f);
            }
            None => {
                for col in band.iter_mut() {
                    if !col.is_empty() {
                        out.push(merge_rows(std::mem::take(col)));
                    }
                }
                spanning.push(f);
            }
        }
    }
    if !spanning.is_empty() {
        out.push(merge_rows(spanning));
    }
    for col in band {
        if !col.is_empty() {
            out.push(merge_rows(col));
        }
    }
    out
}

/// 把同一栏里基线相同的片段合成一行（片段已按 y 降序排好）。
fn merge_rows(frags: Vec<Line>) -> Vec<Line> {
    let mut rows: Vec<Vec<Line>> = Vec::new();
    for f in frags {
        match rows.last_mut() {
            Some(r)
                if !f.raw && !r[0].raw && (r[0].y - f.y).abs() < 0.35 * r[0].size.max(f.size) =>
            {
                r.push(f)
            }
            _ => rows.push(vec![f]),
        }
    }
    rows.into_iter()
        .filter_map(|mut r| {
            r.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            let mut it = r.into_iter();
            let mut line = it.next()?;
            for f in it {
                let cjk = line.text.chars().last().is_some_and(is_cjk)
                    && f.text.chars().next().is_some_and(is_cjk)
                    && f.x0 - line.x1 < CJK_SPACE_GAP * f.size;
                if !cjk {
                    line.text.push(' ');
                }
                line.text.push_str(&f.text);
                line.x1 = line.x1.max(f.x1);
                if f.text.chars().count() > line.text.chars().count() / 2 {
                    line.size = line.size.max(f.size);
                }
                line.bold &= f.bold;
            }
            Some(line)
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct Para {
    pub(crate) text: String,
    pub(crate) x0: f64,
    pub(crate) size: f64,
    pub(crate) bold: bool,
    pub(crate) lines: usize,
    pub(crate) bullet: bool,
    /// 是块（栏）的第一段，可能是上一栏段落的延续。
    pub(crate) block_start: bool,
    /// 原样输出（表格）。
    pub(crate) raw: bool,
}

const BULLETS: &[char] = &[
    '•', '●', '○', '◦', '▪', '■', '□', '‣', '⁃', '∙', '\u{F0B7}', '\u{F0A7}',
];

fn is_bullet_start(text: &str) -> bool {
    let mut cs = text.chars();
    cs.next().is_some_and(|c| BULLETS.contains(&c))
}

fn ends_terminal(text: &str) -> bool {
    let t = text.trim_end_matches(|c: char| {
        matches!(
            c,
            ')' | '"' | '\'' | '”' | '’' | '」' | '』' | '）' | ']' | '*'
        )
    });
    t.ends_with(['.', '!', '?', ':', ';', '。', '！', '？', '：', '；', '…'])
}

fn starts_lower(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_lowercase)
}

/// 把下一行接到段落后面：处理行尾连字符、中日文不加空格。
pub(crate) fn join_text(a: &mut String, b: &str) {
    if a.ends_with('\u{AD}') {
        a.pop();
        a.push_str(b);
        return;
    }
    if (a.ends_with('-') || a.ends_with('\u{2010}')) && starts_lower(b) {
        let before: Vec<char> = a.chars().rev().take(2).collect();
        if before.get(1).is_some_and(|c| c.is_alphabetic()) {
            let last_word = a.rsplit(' ').next().unwrap_or("");
            let inner = last_word
                .strip_suffix(['-', '\u{2010}'])
                .unwrap_or(last_word);
            // 复合词（state-of-the-art）保留连字符
            if !inner.contains('-') {
                a.pop();
            }
            a.push_str(b);
            return;
        }
    }
    let cjk = a.chars().last().is_some_and(is_cjk) && b.chars().next().is_some_and(is_cjk);
    if !cjk && !a.is_empty() {
        a.push(' ');
    }
    a.push_str(b);
}

/// 前一段没结束、这一段像是接着写的：跨栏、跨页时合并。
pub(crate) fn continues(prev: &Para, cur: &Para) -> bool {
    if prev.raw || cur.raw || prev.bullet || cur.bullet || cur.bold != prev.bold {
        return false;
    }
    let ratio = cur.size / prev.size;
    if !(0.9..=1.1).contains(&ratio) || ends_terminal(&prev.text) {
        return false;
    }
    starts_lower(&cur.text)
        || (prev.text.chars().last().is_some_and(is_cjk)
            && cur.text.chars().next().is_some_and(is_cjk))
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// 一页的块切成段落，并把跨栏延续的段落接起来。
pub(crate) fn paragraphs(blocks: Vec<Vec<Line>>) -> Vec<Para> {
    // 整页的典型行距，用来判断段间距
    let mut diffs = Vec::new();
    for b in &blocks {
        for w in b.windows(2) {
            let d = w[0].y - w[1].y;
            let s = w[0].size.max(w[1].size);
            if d > 0.5 * s && d < 3.0 * s && (w[0].size / w[1].size - 1.0).abs() < 0.15 {
                diffs.push(d / s);
            }
        }
    }
    let leading_ratio = median(diffs).unwrap_or(1.2);

    let mut out: Vec<Para> = Vec::new();
    for b in blocks {
        let left = b.iter().map(|l| l.x0).fold(f64::INFINITY, f64::min);
        let right = b.iter().map(|l| l.x1).fold(f64::NEG_INFINITY, f64::max);
        let mut cur: Option<Para> = None;
        let mut prev: Option<&Line> = None;
        let mut first_in_block = true;
        for line in &b {
            if line.raw {
                if let Some(para) = cur.take() {
                    out.push(para);
                }
                out.push(Para {
                    text: line.text.clone(),
                    x0: line.x0,
                    size: line.size,
                    bold: false,
                    lines: line.text.lines().count(),
                    bullet: false,
                    block_start: false,
                    raw: true,
                });
                prev = None;
                first_in_block = false;
                continue;
            }
            let brk = match prev {
                None => true,
                Some(p) => {
                    let gap = p.y - line.y;
                    let s = p.size.max(line.size);
                    let ratio = line.size / p.size;
                    let short = p.x1 < right - (3.0 * p.size).max(0.15 * (right - left));
                    !(0.87..=1.15).contains(&ratio)
                        || line.bold != p.bold
                        || gap > leading_ratio * s * 1.35
                        || gap < -0.3 * s
                        || is_bullet_start(&line.text)
                        || (line.x0 > left + 0.8 * line.size && p.x0 <= left + 0.5 * p.size)
                        || (short && (ends_terminal(&p.text) || line.x0 < p.x0 - 0.8 * s))
                }
            };
            if brk {
                if let Some(para) = cur.take() {
                    out.push(para);
                }
                cur = Some(Para {
                    text: line.text.clone(),
                    x0: line.x0,
                    size: line.size,
                    bold: line.bold,
                    lines: 1,
                    bullet: is_bullet_start(&line.text),
                    block_start: first_in_block,
                    raw: false,
                });
                first_in_block = false;
            } else if let Some(para) = cur.as_mut() {
                join_text(&mut para.text, &line.text);
                para.lines += 1;
            }
            prev = Some(line);
        }
        if let Some(para) = cur {
            out.push(para);
        }
    }
    merge_continuations(out)
}

fn merge_continuations(paras: Vec<Para>) -> Vec<Para> {
    let mut out: Vec<Para> = Vec::new();
    for p in paras {
        if p.block_start
            && let Some(last) = out.last_mut()
            && continues(last, &p)
        {
            join_text(&mut last.text, &p.text);
            last.lines += p.lines;
            continue;
        }
        out.push(p);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Heading(usize),
    Bullet,
    Body,
    Raw,
}

/// 统计全文正文字号和标题层级，生成每页的 Markdown 和合并后的全文。
pub(crate) fn render(pages: &[Vec<Para>]) -> (Vec<String>, String) {
    let mut hist: HashMap<i64, usize> = HashMap::new();
    let (mut bold_chars, mut all_chars) = (0usize, 0usize);
    for p in pages.iter().flatten().filter(|p| !p.raw) {
        let n = p.text.chars().count();
        *hist.entry((p.size * 2.0).round() as i64).or_default() += n;
        all_chars += n;
        if p.bold {
            bold_chars += n;
        }
    }
    let body = hist
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(k, _)| *k as f64 / 2.0)
        .unwrap_or(10.0);
    let body_bold = bold_chars * 2 > all_chars;

    let is_heading_size = |p: &Para| {
        p.size >= body * 1.15
            && p.size - body >= 1.0
            && p.lines <= 4
            && p.text.chars().count() <= 200
            && p.text.chars().any(char::is_alphabetic)
            && !p.bullet
    };
    let mut levels: Vec<f64> = pages
        .iter()
        .flatten()
        .filter(|p| !p.raw && is_heading_size(p))
        .map(|p| p.size)
        .collect();
    levels.sort_by(|a, b| b.total_cmp(a));
    levels.dedup_by(|a, b| (*b - *a).abs() < 0.5);

    // 粗体小标题：不比下一段更靠右（排除 man 手册里缩进的粗体正文）
    let kind = |p: &Para, next: Option<&Para>| -> Kind {
        if p.raw {
            return Kind::Raw;
        }
        if is_heading_size(p) {
            let lvl = levels.iter().position(|&s| p.size >= s - 0.25).unwrap_or(0);
            return Kind::Heading((lvl + 1).min(6));
        }
        if p.bullet {
            return Kind::Bullet;
        }
        if p.bold
            && !body_bold
            && p.lines == 1
            && p.text.chars().count() <= 80
            && p.size >= body * 0.95
            && !p
                .text
                .ends_with(['.', ',', ';', ':', '。', '，', '；', '：'])
            && p.text.chars().any(char::is_alphabetic)
            && next.is_none_or(|n| n.x0 >= p.x0 - 0.5 * p.size)
        {
            return Kind::Heading((levels.len() + 1).min(6));
        }
        Kind::Body
    };
    let kinds: Vec<Vec<Kind>> = pages
        .iter()
        .map(|ps| (0..ps.len()).map(|i| kind(&ps[i], ps.get(i + 1))).collect())
        .collect();

    let page_md: Vec<String> = pages
        .iter()
        .zip(&kinds)
        .map(|(ps, ks)| {
            ps.iter()
                .zip(ks)
                .map(|(p, &k)| format_para(p, k))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .collect();

    // 全文：跨页的段落延续要接起来
    let mut all: Vec<(Para, Kind)> = Vec::new();
    for (ps, ks) in pages.iter().zip(&kinds) {
        for (i, (p, &k)) in ps.iter().zip(ks).enumerate() {
            if i == 0
                && k == Kind::Body
                && let Some((last, Kind::Body)) = all.last_mut()
                && continues(last, p)
            {
                join_text(&mut last.text, &p.text);
                last.lines += p.lines;
                continue;
            }
            all.push((p.clone(), k));
        }
    }
    let mut doc = all
        .iter()
        .map(|(p, k)| format_para(p, *k))
        .collect::<Vec<_>>()
        .join("\n\n");
    if !doc.is_empty() {
        doc.push('\n');
    }
    (page_md, doc)
}

/// 相邻两段公式只隔一个空格（公式跨了两个行片段）时合成一段：`$a \\leq$ $b$` → `$a \\leq b$`。
fn merge_math(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("$ $") {
        return text.into();
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut open = false;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            if open && b.get(i + 1) == Some(&b' ') && b.get(i + 2) == Some(&b'$') {
                out.push(' ');
                i += 3;
                continue;
            }
            open = !open;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out.into()
}

fn format_para(p: &Para, kind: Kind) -> String {
    let text = merge_math(&p.text);
    let p = &Para {
        text: text.into_owned(),
        ..p.clone()
    };
    match kind {
        Kind::Heading(l) => format!("{} {}", "#".repeat(l), p.text),
        Kind::Bullet => {
            let rest = p.text.trim_start_matches(BULLETS).trim_start();
            format!("- {rest}")
        }
        Kind::Raw => p.text.clone(),
        Kind::Body => {
            if p.text.starts_with('#') {
                format!("\\{}", p.text)
            } else {
                p.text.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_formulas_merge() {
        assert_eq!(
            merge_math("with $0 \\leq k \\leq$ $2^{N}$. Then $a$ and $b$"),
            "with $0 \\leq k \\leq 2^{N}$. Then $a$ and $b$"
        );
    }

    #[test]
    fn hyphen_join() {
        let mut a = "an extra-".to_string();
        join_text(&mut a, "ordinary day");
        assert_eq!(a, "an extraordinary day");
        let mut a = "state-of-the-".to_string();
        join_text(&mut a, "art");
        assert_eq!(a, "state-of-the-art");
        let mut a = "中文".to_string();
        join_text(&mut a, "句子");
        assert_eq!(a, "中文句子");
        let mut a = "co\u{2010}".to_string();
        join_text(&mut a, "operate");
        assert_eq!(a, "cooperate");
        let mut a = "Version 2-".to_string();
        join_text(&mut a, "Beta");
        assert_eq!(a, "Version 2- Beta");
    }

    #[test]
    fn furniture_keys() {
        assert_eq!(furniture_key("Page 12 of 30"), "page # of #");
        assert!(is_page_number(&furniture_key("- 7 -")));
        assert!(is_page_number(&furniture_key("xiv")));
        assert!(!is_page_number(&furniture_key("Chapter 3")));
    }
}
