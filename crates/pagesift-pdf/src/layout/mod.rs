//! 版面重组：字形 → 行片段 → 分栏阅读顺序 → 段落 → Markdown。

use std::collections::HashMap;

use crate::interp::Glyph;

mod furniture;
mod headings;
mod render;

pub(crate) use furniture::remove_furniture;
#[cfg(test)]
use furniture::{furniture_key, is_page_number};
#[cfg(test)]
use render::merge_math;
pub(crate) use render::{Mode, Para, join_text, paragraphs, render};

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
    /// 多数字符所在标记内容的 MCID。
    pub(crate) mcid: Option<i32>,
    /// 多数字符在 /Artifact 里。
    pub(crate) artifact: bool,
    /// 写入方给出的标题层级（来自结构树）；None 表示没有提示。
    pub(crate) hint: Option<u8>,
    /// 标题提示来自哪个结构元素：同一元素的行才能合成一段。
    pub(crate) group: Option<u32>,
    /// 写入方声明过的标题行（对上了这一页的书签）：不当页眉页脚删。
    pub(crate) keep: bool,
}

/// 片段内超过这个间距（相对字号）就拆开，拆多了没关系，分栏后会按基线合回去。
const SPLIT_GAP: f64 = 0.45;
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

/// 把一页的字形聚合成行片段。`heading_of` 给出 MCID 所属的标题元素（结构树）：
/// 同一条基线上属于不同标题元素（或一边是标题一边不是）的字形分成不同片段。
pub(crate) fn fragments(glyphs: &[Glyph], heading_of: Option<&HashMap<i32, (u8, u32)>>) -> Vec<Line> {
    let group = |g: &Glyph| heading_of.and_then(|h| g.mcid.and_then(|m| h.get(&m))).map(|&(_, e)| e);
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
                if gap > SPLIT_GAP * g.size.max(p.size) || (heading_of.is_some() && group(p) != group(g)) {
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
    let mut mcids: HashMap<i32, usize> = HashMap::new();
    let mut artifact_chars = 0usize;
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
        if let Some(m) = g.mcid {
            *mcids.entry(m).or_default() += n;
        }
        if g.artifact {
            artifact_chars += n;
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
        mcid: mcids
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            .map(|(m, _)| m),
        artifact: artifact_chars * 2 > all_chars,
        hint: None,
        group: None,
        keep: false,
    })
}

/// 找分栏的栏间空白，返回每条空白的 [左, 右]。
fn gutters(frags: &[Line]) -> Vec<(f64, f64)> {
    gutters_of(&frags.iter().collect::<Vec<_>>())
}

/// 同 `gutters`，输入是行的引用（表格检测只看还没归表格的片段）。
pub(crate) fn gutters_of(frags: &[&Line]) -> Vec<(f64, f64)> {
    let left = frags.iter().map(|f| f.x0).fold(f64::INFINITY, f64::min);
    let right = frags.iter().map(|f| f.x1).fold(f64::NEG_INFINITY, f64::max);
    let width = right - left;
    if width.is_nan() || width <= 1.0 || frags.len() < 6 {
        return Vec::new();
    }
    // 原样输出的块（表格、图内文字）不当候选
    let cands: Vec<&Line> = frags
        .iter()
        .copied()
        .filter(|f| f.x1 - f.x0 < 0.55 * width && !f.raw)
        .collect();
    if cands.len() < 6 {
        return Vec::new();
    }
    let mut sizes: Vec<f64> = cands.iter().map(|f| f.size).collect();
    sizes.sort_by(f64::total_cmp);
    let med = sizes[sizes.len() / 2];
    let min_gap = (0.55 * med).max(3.5);

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
    let tol = (cands.len() / 15).max(1);
    let out = scan(&cands, &cover, tol, left, scale, min_gap, None);
    if !out.is_empty() {
        return out;
    }
    // 一条也没找到时再放宽：只看两栏正文（正文字号的行）并排的那段高度，那里没有行跨过空白就算栏间空白
    // （图上方跨栏的小标题、图例不在这段高度里）
    scan(&cands, &cover, (cands.len() / 4).max(1), left, scale, min_gap, Some((tol, med)))
}

/// 在覆盖直方图里找不超过 `tol` 的空白段。`band_tol`（容差, 正文字号）给出时，再核对两边正文字号的行
/// 并排的高度范围里，跨过空白的行不超过容差。
fn scan(
    cands: &[&Line],
    cover: &[usize],
    tol: usize,
    left: f64,
    scale: f64,
    min_gap: f64,
    band_tol: Option<(usize, f64)>,
) -> Vec<(f64, f64)> {
    let bins = cover.len() - 1;
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
                let lhs: Vec<&&Line> = cands.iter().filter(|f| f.x1 <= g0 + 0.5).collect();
                let rhs: Vec<&&Line> = cands.iter().filter(|f| f.x0 >= g1 - 0.5).collect();
                let need = (cands.len() / 10).max(3);
                if lhs.len() < need || rhs.len() < need {
                    continue;
                }
                if let Some((band_tol, med)) = band_tol {
                    // 两边都得是正文栏：正文字号的行宽中位数不小于全宽的四分之一（不是编号列、作者名格子）
                    let full = bins as f64 / scale;
                    let wide = |v: &[&&Line]| {
                        let mut w: Vec<f64> = v.iter().filter(|f| (f.size - med).abs() < 0.5).map(|f| f.x1 - f.x0).collect();
                        w.sort_by(f64::total_cmp);
                        w.get(w.len() / 2).is_some_and(|&m| m >= 0.25 * full)
                    };
                    if !wide(&lhs) || !wide(&rhs) {
                        continue;
                    }
                    let span = |v: &[&&Line]| {
                        v.iter()
                            .filter(|f| (f.size - med).abs() < 0.5)
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), f| (lo.min(f.y), hi.max(f.y)))
                    };
                    let ((l0, l1), (r0, r1)) = (span(&lhs), span(&rhs));
                    let (lo, hi) = (l0.max(r0), l1.min(r1));
                    let crossing = cands
                        .iter()
                        .filter(|f| f.x0 < g0 - 0.5 && f.x1 > g1 + 0.5 && f.y >= lo && f.y <= hi)
                        .count();
                    if lo >= hi || crossing > band_tol {
                        continue;
                    }
                }
                out.push((g0, g1));
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
            if f.x0 < g0 - 8.0 && f.x1 > g1 + 8.0 {
                return None;
            }
            if (f.x0 + f.x1) / 2.0 >= mid {
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
                if !f.raw
                    && !r[0].raw
                    && r[0].group == f.group
                    && (r[0].y - f.y).abs() < 0.35 * r[0].size.max(f.size) =>
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
