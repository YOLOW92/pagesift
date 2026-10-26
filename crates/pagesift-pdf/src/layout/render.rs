use std::borrow::Cow;
use std::collections::HashMap;

use super::{Line, headings, is_cjk};
use crate::structure::{Style, numbering};

/// 以章节编号开头、像标题的一行：编号后有文字、不以句末标点结尾、没有目录的点线，
/// 而且不是一段正文的首行（伸到栏的右边缘、下一行接着写的是折行）。
fn numbered_line(
    line: &Line,
    prev: Option<&Line>,
    prev_numbered: bool,
    next: Option<&Line>,
    right: f64,
) -> Option<Style> {
    // 标题前面的段落已经结束（有句末标点），或者前一行本身就是标题。上一行没有句末标点、
    // 行距是正常行距（不是段间距）：这一行是句子折过来的（`… in more detail in` / `Section 3. In …`、
    // 绕图排的窄栏里 `… was opened on` / `8 June 1905. …`）
    if !prev_numbered
        && prev.is_some_and(|p| {
            !ends_terminal(&p.text)
                && ((p.y - line.y) < 1.5 * p.size || p.x1 >= right - 2.0 * p.size)
                && (p.y - line.y) < 2.0 * p.size
                && (p.size / line.size - 1.0).abs() < 0.1
        })
    {
        return None;
    }
    let (style, rest) = numbering(&line.text)?;
    let rest = rest.trim();
    // 单个字母的编号只认附录常用的 A–F，而且后面不能有逗号：`M. Franko, 2/15/2018` 是姓名缩写
    if style == Style::Letter(1)
        && (!line
            .text
            .trim_start()
            .starts_with(['A', 'B', 'C', 'D', 'E', 'F'])
            || rest.contains(','))
    {
        return None;
    }
    if rest.chars().filter(|c| c.is_alphabetic()).count() < 2
        || rest.ends_with(['。', '.', '．', '、', ',', '，', ';', '；'])
        || rest.contains(". . .")
        || rest.contains("...")
        || rest.contains('…')
    {
        return None;
    }
    let cjk = rest.chars().filter(|&c| is_cjk(c)).count() * 2 > rest.chars().count();
    let long = if cjk {
        rest.chars().count() > 40
    } else {
        rest.split_whitespace().count() > 14
    };
    let whole_line = matches!(style, Style::Lenticular | Style::Angle);
    let wrapped = line.x1 >= right - 2.0 * line.size
        && next.is_some_and(|n| {
            numbering(&n.text).is_none()
                && (line.y - n.y) < 2.0 * line.size
                && (n.size / line.size - 1.0).abs() < 0.1
        });
    if long || (!whole_line && wrapped) {
        return None;
    }
    Some(style)
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
    /// 首行基线的 y（页面坐标，向上为正）。
    pub(crate) top: f64,
    /// 写入方给出的标题层级（结构树或书签）。
    pub(crate) hint: Option<u8>,
    /// 标题提示来自哪个结构元素。
    pub(crate) group: Option<u32>,
    /// 单独成行、以章节编号开头的短行（推断标题用）。
    pub(crate) numbered: Option<Style>,
    /// 由旋转的文字组成（坐标轴标题、页边标签）：不当标题，也不和水平的行合段。
    pub(crate) turned: bool,
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
    if prev.raw
        || cur.raw
        || prev.bullet
        || cur.bullet
        || cur.bold != prev.bold
        || cur.hint != prev.hint
        || cur.group != prev.group
        || cur.numbered.is_some()
        || prev.numbered.is_some()
    {
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
        let mut prev_numbered = false;
        let mut first_in_block = true;
        for (li, line) in b.iter().enumerate() {
            let numbered = numbered_line(
                line,
                li.checked_sub(1).and_then(|k| b.get(k)),
                prev_numbered,
                b.get(li + 1),
                right,
            );
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
                    top: line.y,
                    hint: None,
                    group: None,
                    numbered: None,
                    turned: false,
                });
                prev = None;
                prev_numbered = false;
                first_in_block = false;
                continue;
            }
            // 编号标题行单独成段：前后都断开（解决标题和下一行正文粘连）
            let brk = numbered.is_some()
                || prev_numbered
                || match prev {
                    None => true,
                    Some(p) => {
                        let gap = p.y - line.y;
                        let s = p.size.max(line.size);
                        let ratio = line.size / p.size;
                        let short = p.x1 < right - (3.0 * p.size).max(0.15 * (right - left));
                        !(0.87..=1.15).contains(&ratio)
                            || line.hint != p.hint
                            || line.group != p.group
                            || line.turned != p.turned
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
                    top: line.y,
                    hint: line.hint,
                    group: line.group,
                    numbered,
                    turned: line.turned,
                });
                first_in_block = false;
            } else if let Some(para) = cur.as_mut() {
                join_text(&mut para.text, &line.text);
                para.lines += 1;
            }
            prev = Some(line);
            prev_numbered = numbered.is_some();
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

/// 图表的题注（`Figure 1.2.`、`Table 3:`、`図2`、`表1`）和单独的小写罗马数字页码（专栏 `Box 1.1` 是章节级的，不算）：
/// 版面上常和标题一样醒目，但不是章节标题。
fn caption_or_folio(text: &str) -> bool {
    let t = text.trim();
    if !t.is_empty() && t.len() <= 6 && t.chars().all(|c| "ivxlc".contains(c)) {
        return true;
    }
    // 只看开头：最长的题注词 12 个字符，再加后面的一个字符
    let head = t.char_indices().nth(24).map_or(t, |(i, _)| &t[..i]);
    let lower = head.to_lowercase();
    let words = [
        "figure",
        "fig.",
        "fig ",
        "table",
        "chart",
        "exhibit",
        "listing",
        "algorithm",
        "graph",
        "annex figure",
        "annex table",
        "online annex",
        "panel",
    ];
    for w in words {
        if let Some(rest) = lower.strip_prefix(w) {
            let rest = rest.trim_start();
            if rest
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
                || head
                    .get(head.len().saturating_sub(rest.len())..)
                    .and_then(|r| r.chars().next())
                    .is_some_and(|c| c.is_ascii_uppercase())
            {
                return true;
            }
        }
    }
    for w in ["図表", "図", "表", "グラフ", "参考図表"] {
        if let Some(rest) = t.strip_prefix(w) {
            let rest = rest.trim_start();
            if rest.chars().next().is_some_and(|c| {
                c.is_ascii_digit() || ('０'..='９').contains(&c) || c == '（' || c == '('
            }) {
                return true;
            }
        }
    }
    false
}

/// 一页里字数最多的字号（按 0.5pt 取整）。
fn body_size(ps: &[Para]) -> Option<f64> {
    let mut hist: HashMap<i64, usize> = HashMap::new();
    for p in ps.iter().filter(|p| !p.raw) {
        *hist.entry((p.size * 2.0).round() as i64).or_default() += p.text.chars().count();
    }
    hist.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(k, _)| k as f64 / 2.0)
}

/// 标题怎么定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// 按字号、粗体推断。
    Infer,
    /// 写入方给了标题（结构树或书签）：只有带提示的段落是标题，层级用提示的。
    Hinted,
}

/// 统计全文正文字号和标题层级，生成每页的 Markdown 和合并后的全文。
pub(crate) fn render(pages: &[Vec<Para>], mode: Mode) -> (Vec<String>, String) {
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
    // 推断：字号明显大、正文字号的单行粗体（不比下一段更靠右，排除 man 手册里缩进的粗体正文）、
    // 单独成行的编号标题。层级统一交给 headings::levels。
    // 比较“不比正文小”时，整页都是小字的页（决算短信的摘要页）用本页的正文字号，
    // 否则那几页的标题都会被当成脚注；页上有正文字号的文字时照旧用全文的（图表页的小字不能算标题）
    let is_heading = |p: &Para, next: Option<&Para>, page_body: f64| -> bool {
        if p.raw || p.bullet || p.turned || caption_or_folio(&p.text) {
            return false;
        }
        if is_heading_size(p) {
            return true;
        }
        let one_line = p.lines == 1
            && p.size >= page_body.min(body) * 0.95
            && p.text.chars().any(char::is_alphabetic);
        let bold = p.bold
            && !body_bold
            && one_line
            && p.text.chars().count() <= 80
            && !p
                .text
                .ends_with(['.', ',', ';', ':', '。', '，', '；', '：'])
            && next.is_none_or(|n| n.x0 >= p.x0 - 0.5 * p.size);
        let numbered = p.numbered.is_some() && one_line && p.text.chars().count() <= 120;
        bold || numbered
    };
    // 相邻两段都是同一种编号、中间没有正文：这是列表，不是标题（只影响单靠编号判出的）
    let in_list = |ps: &[Para], i: usize| -> bool {
        let Some(style) = ps[i].numbered else {
            return false;
        };
        let same = |j: usize| ps.get(j).is_some_and(|q| q.numbered == Some(style));
        same(i + 1) || (i > 0 && same(i - 1))
    };
    let mut cands: Vec<headings::Cand> = Vec::new();
    let mut is_cand: Vec<Vec<bool>> = Vec::with_capacity(pages.len());
    for ps in pages {
        let has_body_text = ps.iter().any(|p| !p.raw && (p.size - body).abs() < 0.75);
        let page_body = if has_body_text {
            body
        } else {
            body_size(ps).unwrap_or(body)
        };
        let mut row = Vec::with_capacity(ps.len());
        for (i, p) in ps.iter().enumerate() {
            let h = mode == Mode::Infer
                && is_heading(p, ps.get(i + 1), page_body)
                && (is_heading_size(p) || (p.bold && !body_bold) || !in_list(ps, i));
            if h {
                cands.push(headings::Cand {
                    size: p.size,
                    style: numbering(&p.text).map(|(s, _)| s),
                });
            }
            row.push(h);
        }
        is_cand.push(row);
    }
    let mut levels = headings::levels(&cands).into_iter();
    let kinds: Vec<Vec<Kind>> = pages
        .iter()
        .zip(&is_cand)
        .map(|(ps, hs)| {
            ps.iter()
                .zip(hs)
                .map(|(p, &h)| {
                    if p.raw {
                        Kind::Raw
                    } else if mode == Mode::Hinted && p.hint.is_some() {
                        Kind::Heading(usize::from(p.hint.unwrap_or(1)).clamp(1, 6))
                    } else if h {
                        Kind::Heading(levels.next().unwrap_or(1))
                    } else if p.bullet {
                        Kind::Bullet
                    } else {
                        Kind::Body
                    }
                })
                .collect()
        })
        .collect();

    // 只有编号的标题（`CHAPTER 2`、`C.`）：版式上常把章号和章名分两行排，逻辑上是一个标题。
    // 下一段也是标题就并进去，否则不当标题（不输出空标题）
    // 只复制真要改的页
    let label_only = |p: &Para, k: Kind| {
        matches!(k, Kind::Heading(_))
            && crate::structure::strip_numbering(&p.text).trim().is_empty()
    };
    let mut kinds = kinds;
    let mut owned: Vec<Cow<'_, [Para]>> = Vec::with_capacity(pages.len());
    for (ps, ks) in pages.iter().zip(kinds.iter_mut()) {
        if !ps.iter().zip(ks.iter()).any(|(p, &k)| label_only(p, k)) {
            owned.push(Cow::Borrowed(ps.as_slice()));
            continue;
        }
        let mut ps = ps.clone();
        let mut i = 0;
        while i < ps.len() {
            if label_only(&ps[i], ks[i]) {
                if matches!(ks.get(i + 1), Some(Kind::Heading(_))) {
                    let label = ps.remove(i).text;
                    ks.remove(i);
                    ps[i].text = format!("{} {}", label.trim(), ps[i].text);
                    continue;
                }
                ks[i] = Kind::Body;
            }
            i += 1;
        }
        owned.push(Cow::Owned(ps));
    }
    let pages = &owned[..];
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
    // 只有接上了下一页的段落才复制
    let mut all: Vec<(Cow<'_, Para>, Kind)> = Vec::new();
    for (ps, ks) in pages.iter().zip(&kinds) {
        for (i, (p, &k)) in ps.iter().zip(ks).enumerate() {
            if i == 0
                && k == Kind::Body
                && let Some((last, Kind::Body)) = all.last_mut()
                && continues(last, p)
            {
                let last = last.to_mut();
                join_text(&mut last.text, &p.text);
                last.lines += p.lines;
                continue;
            }
            all.push((Cow::Borrowed(p), k));
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
    match kind {
        Kind::Heading(l) => format!("{} {}", "#".repeat(l), text),
        Kind::Bullet => {
            let rest = text.trim_start_matches(BULLETS).trim_start();
            format!("- {rest}")
        }
        Kind::Raw => text.into_owned(),
        Kind::Body => {
            if text.starts_with('#') {
                format!("\\{}", text)
            } else {
                text.into_owned()
            }
        }
    }
}
