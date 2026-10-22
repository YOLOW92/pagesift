use std::collections::HashMap;

use super::{Line, is_cjk};

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
pub(super) fn merge_math(text: &str) -> std::borrow::Cow<'_, str> {
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
