//! DOM 子树转 GFM。分两层：块级渲染产出段落列表，行内渲染产出单行文本。

use pagesift_html::{Element, Tag};
use url::Url;

use crate::dom::{NodeR, collapse_whitespace, collapse_whitespace_into, element, raw_text, tag};
use crate::extract::Skipper;
use crate::math::{self, Math};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Para,
    List,
    Other,
}

struct Block {
    kind: Kind,
    text: String,
}

pub(crate) struct Renderer<'a> {
    pub base: Option<&'a Url>,
    pub skipper: &'a Skipper,
    pub whitelist: Whitelist,
}

/// 只输出哪些元素里的文字（照 trafilatura 的 TAG_CATALOG）。文章模式里，散落在 `div`、`span`、
/// `section` 里的行内文字（署名、日期、图注、按钮、"阅读更多"）不是正文段落。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Whitelist {
    /// 所有容器的文字都输出（默认）。
    Off,
    /// 只输出段落、标题、列表项、引用、代码、表格单元格里的文字；`div` 为真时
    /// 只装文字的 `div` 也算段落（页面不用 `<p>` 排版时）。
    On { div: bool },
}

impl<'a> Renderer<'a> {
    pub(crate) fn new(base: Option<&'a Url>, skipper: &'a Skipper) -> Self {
        Self {
            base,
            skipper,
            whitelist: Whitelist::Off,
        }
    }

    pub(crate) fn whitelist(mut self, w: Whitelist) -> Self {
        self.whitelist = w;
        self
    }

    /// `node` 里直接写的行内文字能不能成为一个段落。
    fn paragraph_allowed(&self, node: &NodeR<'_>) -> bool {
        match self.whitelist {
            Whitelist::Off => true,
            Whitelist::On { div } => match node.tag() {
                Some(
                    Tag::P
                    | Tag::Blockquote
                    | Tag::Li
                    | Tag::Dd
                    | Tag::Dt
                    | Tag::Td
                    | Tag::Th
                    | Tag::Pre
                    | Tag::Code,
                ) => true,
                Some(Tag::Div) => div,
                _ => false,
            },
        }
    }
}

pub(crate) fn is_block_tag(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        Address
            | Article
            | Aside
            | Blockquote
            | Center
            | Details
            | Dd
            | Dialog
            | Div
            | Dl
            | Dt
            | Fieldset
            | Figcaption
            | Figure
            | Footer
            | Form
            | H1
            | H2
            | H3
            | H4
            | H5
            | H6
            | Header
            | Hgroup
            | Hr
            | Li
            | Main
            | Nav
            | Ol
            | P
            | Pre
            | Section
            | Summary
            | Table
            | Tbody
            | Td
            | Tfoot
            | Th
            | Thead
            | Tr
            | Ul
            | Body
            | Html
    )
}

fn is_inline_tag(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        A | Abbr
            | B
            | Bdi
            | Bdo
            | Br
            | Cite
            | Code
            | Data
            | Del
            | Dfn
            | Em
            | I
            | Img
            | Ins
            | Kbd
            | Label
            | Mark
            | Q
            | S
            | Samp
            | Small
            | Span
            | Strike
            | Strong
            | Sub
            | Sup
            | Time
            | Tt
            | U
            | Var
            | Wbr
            | Font
            | Picture
    )
}

impl Renderer<'_> {
    pub(crate) fn render(&self, roots: &[NodeR<'_>]) -> String {
        let mut blocks = Vec::new();
        for root in roots {
            if root.is_element() && self.acts_as_block(root) {
                self.block(*root, &mut blocks);
            } else {
                blocks.extend(self.blocks_of(*root));
            }
        }
        if self.skipper.only_main() {
            trim_leading_chrome(&mut blocks);
        }
        let mut out = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// 公式节点（页面里不可能有公式时直接跳过检查）。
    fn math(&self, node: &NodeR<'_>) -> Option<Math> {
        if self.skipper.has_math() {
            math::math_of(node)
        } else {
            None
        }
    }

    fn skip(&self, node: &NodeR<'_>) -> bool {
        if node.is_comment() {
            return true;
        }
        // 公式在 `<math>`、`<script>`、隐藏的 span 里，照常的跳过规则会把它删掉
        node.is_element() && self.skipper.skip(node) && self.math(node).is_none()
    }

    /// 元素按块处理还是按行内处理。未知元素（自定义元素等）看里面有没有块级内容。
    fn acts_as_block(&self, node: &NodeR<'_>) -> bool {
        match self.math(node) {
            Some(Math::Tex(_, display)) => return display,
            Some(Math::Skip) => return false,
            None => {}
        }
        match node.tag() {
            Some(t) if is_block_tag(t) => true,
            Some(t) if is_inline_tag(t) => self.skipper.has_block_descendant(node),
            Some(_) => self.skipper.has_block_descendant(node),
            None => false,
        }
    }

    /// 容器的子节点：连续的行内内容合成一个段落，遇到块级子元素就断开。
    /// 白名单模式下，不在允许元素里的行内文字丢掉。
    fn blocks_of(&self, node: NodeR<'_>) -> Vec<Block> {
        let mut blocks = Vec::new();
        let mut inline = String::new();
        let keep = self.paragraph_allowed(&node);
        for child in node.children() {
            if self.skip(&child) {
                continue;
            }
            if child.is_element() && self.acts_as_block(&child) {
                if keep {
                    flush_paragraph(&mut inline, &mut blocks);
                } else {
                    inline.clear();
                }
                self.block(child, &mut blocks);
            } else {
                self.inline_into(child, &mut inline);
            }
        }
        if keep {
            flush_paragraph(&mut inline, &mut blocks);
        }
        blocks
    }

    fn block(&self, node: NodeR<'_>, out: &mut Vec<Block>) {
        if let Some(m) = self.math(&node) {
            if let Math::Tex(tex, _) = m {
                out.push(Block {
                    kind: Kind::Other,
                    text: format!("$$\n{tex}\n$$"),
                });
            }
            return;
        }
        let name = tag(&node).unwrap_or("");
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = (name.as_bytes()[1] - b'0') as usize;
                let text = single_line(&self.inline_children(node));
                if !text.is_empty() {
                    out.push(Block {
                        kind: Kind::Other,
                        text: format!("{} {}", "#".repeat(level), text),
                    });
                }
            }
            "hr" => out.push(Block {
                kind: Kind::Other,
                text: "---".into(),
            }),
            "pre" => {
                if let Some(b) = self.code_block(node) {
                    out.push(b);
                }
            }
            "blockquote" => {
                let inner = join_blocks(&self.blocks_of(node));
                if !inner.is_empty() {
                    let text = inner
                        .lines()
                        .map(|l| {
                            if l.is_empty() {
                                ">".to_string()
                            } else {
                                format!("> {l}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push(Block {
                        kind: Kind::Other,
                        text,
                    });
                }
            }
            "ul" | "ol" => {
                if let Some(b) = self.list(node, name == "ol") {
                    out.push(b);
                }
            }
            "table" => self.table(node, out),
            "dt" => {
                let text = single_line(&self.inline_children(node));
                if !text.is_empty() {
                    out.push(Block {
                        kind: Kind::Para,
                        text: format!("**{text}**"),
                    });
                }
            }
            _ => out.extend(self.blocks_of(node)),
        }
    }

    fn code_block(&self, node: NodeR<'_>) -> Option<Block> {
        let text = raw_text(&node);
        let text = text.trim_matches('\n');
        if text.trim().is_empty() {
            return None;
        }
        let lang = code_language(&node).unwrap_or_default();
        let mut fence = "```".to_string();
        while text.contains(&fence) {
            fence.push('`');
        }
        Some(Block {
            kind: Kind::Other,
            text: format!("{fence}{lang}\n{}\n{fence}", text.trim_end()),
        })
    }

    fn list(&self, node: NodeR<'_>, ordered: bool) -> Option<Block> {
        let mut n: i64 = element(&node)
            .and_then(|e| e.attr("start"))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let mut items = Vec::new();
        let mut loose = false;
        for child in node.children() {
            if self.skip(&child) {
                continue;
            }
            let blocks = if tag(&child) == Some("li") {
                self.blocks_of(child)
            } else if child.is_element() {
                // ul 里直接放的非 li 元素：按内容处理，不丢文本
                let mut v = Vec::new();
                if self.acts_as_block(&child) {
                    self.block(child, &mut v);
                } else {
                    let mut s = String::new();
                    self.inline_into(child, &mut s);
                    flush_paragraph(&mut s, &mut v);
                }
                v
            } else {
                continue;
            };
            if blocks.is_empty() {
                continue;
            }
            if blocks.iter().filter(|b| b.kind != Kind::List).count() > 1 {
                loose = true;
            }
            let mut content = String::new();
            for (i, b) in blocks.iter().enumerate() {
                if i > 0 {
                    content.push_str(if b.kind == Kind::List { "\n" } else { "\n\n" });
                }
                content.push_str(&b.text);
            }
            let marker = if ordered {
                let m = format!("{n}. ");
                n += 1;
                m
            } else {
                "- ".to_string()
            };
            let indent = " ".repeat(marker.len());
            let mut item = String::new();
            for (i, line) in content.lines().enumerate() {
                if i == 0 {
                    item.push_str(&marker);
                    item.push_str(line);
                } else {
                    item.push('\n');
                    if !line.is_empty() {
                        item.push_str(&indent);
                        item.push_str(line);
                    }
                }
            }
            items.push(item);
        }
        if items.is_empty() {
            return None;
        }
        Some(Block {
            kind: Kind::List,
            text: items.join(if loose { "\n\n" } else { "\n" }),
        })
    }

    fn table(&self, node: NodeR<'_>, out: &mut Vec<Block>) {
        let mut rows: Vec<NodeR<'_>> = Vec::new();
        collect_rows(node, &mut rows);
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                row.children()
                    .filter(|c| matches!(tag(c), Some("td" | "th")))
                    .map(|c| {
                        let cell =
                            single_line(&join_blocks(&self.blocks_of(c)).replace("  \n", " "));
                        unescape_block_start(&cell).replace('|', "\\|")
                    })
                    .collect()
            })
            .filter(|r: &Vec<String>| !r.is_empty())
            .collect();
        let cols = cells.iter().map(Vec::len).max().unwrap_or(0);
        // 单行或单列的表格基本是排版用的，不当数据表
        if cells.len() < 2 || cols < 2 {
            out.extend(self.blocks_of(node));
            return;
        }
        let fmt_row = |r: &Vec<String>| {
            let mut s = String::from("|");
            for i in 0..cols {
                s.push(' ');
                s.push_str(r.get(i).map(String::as_str).unwrap_or(""));
                s.push_str(" |");
            }
            s
        };
        let mut lines = vec![fmt_row(&cells[0])];
        lines.push(format!("|{}", " --- |".repeat(cols)));
        lines.extend(cells[1..].iter().map(fmt_row));
        out.push(Block {
            kind: Kind::Other,
            text: lines.join("\n"),
        });
    }

    fn inline_children(&self, node: NodeR<'_>) -> String {
        let mut s = String::new();
        self.inline_children_into(node, &mut s);
        s
    }

    fn inline_children_into(&self, node: NodeR<'_>, out: &mut String) {
        for child in node.children() {
            if !self.skip(&child) {
                self.inline_into(child, out);
            }
        }
    }

    /// 行内渲染，直接写进 `out`；只有需要包裹的元素（加粗、链接等）才先渲染到临时串。
    fn inline_into(&self, node: NodeR<'_>, out: &mut String) {
        if let Some(t) = node.as_text() {
            push_text(t, out, self.skipper.has_math());
            return;
        }
        let Some(el) = element(&node) else {
            return;
        };
        if let Some(m) = self.math(&node) {
            if let Math::Tex(tex, display) = m {
                let d = if display { "$$" } else { "$" };
                out.push_str(d);
                out.push_str(&tex);
                out.push_str(d);
            }
            return;
        }
        if self.skip(&node) {
            return;
        }
        match el.tag() {
            Tag::Br => out.push('\n'),
            Tag::Wbr => {}
            Tag::Img => out.push_str(&self.image(el)),
            Tag::Strong | Tag::B => out.push_str(&wrap(&self.inline_children(node), "**")),
            Tag::Em | Tag::I => out.push_str(&wrap(&self.inline_children(node), "*")),
            Tag::Del | Tag::S | Tag::Strike => {
                out.push_str(&wrap(&self.inline_children(node), "~~"))
            }
            Tag::Code | Tag::Kbd | Tag::Samp | Tag::Tt => {
                out.push_str(&inline_code(&collapse_whitespace(&raw_text(&node))))
            }
            Tag::A => out.push_str(&self.link(node, el)),
            t if is_block_tag(t) => {
                out.push(' ');
                self.inline_children_into(node, out);
                out.push(' ');
            }
            _ => self.inline_children_into(node, out),
        }
    }

    fn link(&self, node: NodeR<'_>, el: Element<'_>) -> String {
        let inner = self.inline_children(node);
        let text = single_line(&inner);
        let Some(href) = el.attr("href").map(str::trim) else {
            return inner;
        };
        if href.is_empty()
            || href.starts_with('#')
            || href.to_ascii_lowercase().starts_with("javascript:")
        {
            return inner;
        }
        if text.is_empty() {
            return String::new();
        }
        let url = self.resolve(href);
        let (lead, trail) = edges(&inner);
        format!("{lead}[{text}]({url}){trail}")
    }

    fn image(&self, el: Element<'_>) -> String {
        let Some(src) = image_src(el) else {
            return String::new();
        };
        let alt = single_line(&collapse_whitespace(el.attr("alt").unwrap_or("")));
        format!("![{}]({})", alt.replace(['[', ']'], ""), self.resolve(src))
    }

    fn resolve(&self, href: &str) -> String {
        let resolved = match self.base {
            Some(base) => base.join(href).map(|u| u.to_string()).ok(),
            None => Url::parse(href).map(|u| u.to_string()).ok(),
        };
        let s = resolved.unwrap_or_else(|| href.to_string());
        s.replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
    }
}

/// 图片地址。懒加载的图片 `src` 常是空的或 `data:` 占位图，真实地址在 `data-src`、
/// `data-lazy-src`、`data-original` 或 `srcset` 里；`data:` 地址本身不输出。
fn image_src(el: Element<'_>) -> Option<&str> {
    let usable = |s: &&str| !s.is_empty() && !s.starts_with("data:");
    ["src", "data-src", "data-lazy-src", "data-original"]
        .iter()
        .filter_map(|a| el.attr(a).map(str::trim))
        .find(usable)
        .or_else(|| {
            ["srcset", "data-srcset", "data-lazy-srcset"]
                .iter()
                .filter_map(|a| el.attr(a))
                .filter_map(|v| crate::links::srcset(v).first().map(|c| c.0))
                .find(usable)
        })
}

fn collect_rows<'a>(node: NodeR<'a>, rows: &mut Vec<NodeR<'a>>) {
    for child in node.children() {
        match tag(&child) {
            Some("tr") => rows.push(child),
            Some("table") => {}
            Some(_) => collect_rows(child, rows),
            None => {}
        }
    }
}

fn code_language(pre: &NodeR<'_>) -> Option<String> {
    let mut candidates = vec![*pre];
    candidates.extend(pre.children().filter(|c| tag(c) == Some("code")));
    for n in candidates {
        let Some(el) = element(&n) else { continue };
        for attr in ["data-lang", "data-language"] {
            if let Some(lang) = el.attr(attr).map(str::trim)
                && is_lang_name(lang)
                && !matches!(lang, "plain" | "text" | "plaintext" | "none")
            {
                return Some(lang.to_ascii_lowercase());
            }
        }
        let Some(class) = el.attr("class") else {
            continue;
        };
        for c in class.split_whitespace() {
            for prefix in ["language-", "lang-", "highlight-source-"] {
                if let Some(lang) = c.strip_prefix(prefix)
                    && is_lang_name(lang)
                {
                    return Some(lang.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

fn is_lang_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "+-_#.".contains(ch))
}

fn edges(s: &str) -> (&'static str, &'static str) {
    let lead = if s.starts_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    let trail = if s.ends_with(char::is_whitespace) {
        " "
    } else {
        ""
    };
    (lead, trail)
}

fn wrap(inner: &str, marker: &str) -> String {
    let text = single_line(inner);
    if text.is_empty() {
        return inner.to_string();
    }
    let (lead, trail) = edges(inner);
    format!("{lead}{marker}{text}{marker}{trail}")
}

fn inline_code(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut ticks = "`".to_string();
    while text.contains(&ticks) {
        ticks.push('`');
    }
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

/// 折叠成一行：换行和连续空格都变成单个空格，去掉首尾空白。
fn single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    push_single_line(s, &mut out);
    out
}

fn push_single_line(s: &str, out: &mut String) {
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
    }
}

/// 一段行内内容收成段落。行内的换行来自 `<br>`，写成硬换行（行尾两个空格，看不见，不添噪声）；
/// 每行开头像块级标记（`# `、`- `、`1. `、`> `…）的转义掉，免得被当成标题、列表、引用。
fn flush_paragraph(buf: &mut String, blocks: &mut Vec<Block>) {
    let mut text = String::new();
    for line in buf.split('\n') {
        if line.split_whitespace().next().is_none() {
            continue;
        }
        if !text.is_empty() {
            text.push_str("  \n");
        }
        let start = text.len();
        push_single_line(line, &mut text);
        escape_block_start(&mut text, start);
    }
    buf.clear();
    if !text.is_empty() {
        blocks.push(Block {
            kind: Kind::Para,
            text,
        });
    }
}

/// 文本节点：折叠空白后按 CommonMark 规则做最少的转义（自研）。只在字符真会被解析成格式时才加反斜杠：
/// 成对出现、贴着文字的 `*` `_` `~`（词中间的 `_` 不算）、像标签的 `<`、会构成链接的 `[`、
/// 像实体的 `&`、标点前的 `\`。`a_b_c`、`2 * 3`、`5%` 这类保持原样，大模型读起来也干净。
/// 反引号不转义：网页正文里成对的反引号几乎都是作者写的 Markdown 没被渲染（原意就是代码），
/// 单个的凑不成代码，本来就安全。
fn push_text(t: &str, out: &mut String, tex: bool) {
    let start = out.len();
    collapse_whitespace_into(t, out);
    if !out.as_bytes()[start..]
        .iter()
        .any(|b| matches!(b, b'\\' | b'*' | b'_' | b'~' | b'<' | b'[' | b'&'))
    {
        return;
    }
    let seg = out.split_off(start);
    let chars: Vec<char> = seg.chars().collect();
    // 可能有公式的页面：正文里未渲染的 TeX（MathJax 在浏览器里才渲染）原样保留，不转义
    let protected = if tex { tex_spans(&chars) } else { Vec::new() };
    let count = |c: char| chars.iter().filter(|&&x| x == c).count();
    let (stars, unders, tildes) = (count('*'), count('_'), count('~'));
    let before = out.chars().next_back();
    let ws = |o: Option<char>| o.is_none_or(char::is_whitespace);
    let alnum = |o: Option<char>| o.is_some_and(char::is_alphanumeric);
    for (i, &c) in chars.iter().enumerate() {
        if protected.iter().any(|&(a, b)| a <= i && i < b) {
            out.push(c);
            continue;
        }
        let p = if i > 0 { Some(chars[i - 1]) } else { before };
        let n = chars.get(i + 1).copied();
        let esc = match c {
            '\\' => n.is_some_and(|n| n.is_ascii_punctuation()),
            '*' => stars >= 2 && !(ws(p) && ws(n)),
            '~' => tildes >= 2 && !(ws(p) && ws(n)),
            '_' => unders >= 2 && !(ws(p) && ws(n)) && !(alnum(p) && alnum(n)),
            '<' => n.is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?')),
            '[' => chars[i + 1..]
                .iter()
                .position(|&x| x == ']')
                .is_some_and(|j| matches!(chars.get(i + 2 + j), Some('(' | '['))),
            '&' => entity_like(&chars[i + 1..]),
            _ => false,
        };
        if esc {
            out.push('\\');
        }
        out.push(c);
    }
}

/// 文本里未渲染的 TeX 片段：`$$…$$`、`\[…\]`、`\(…\)`、`$…$`（`$` 后不是空格、前一个 `$` 前不是空格，
/// 结尾 `$` 后面不是数字，免得把 "$5 and $10" 当成公式）。返回字符下标区间。
fn tex_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let find = |from: usize, pat: &[char]| {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&k| chars[k..].starts_with(pat))
    };
    let mut i = 0;
    while i < chars.len() {
        let two = chars.get(i..i + 2);
        let close: Option<&[char]> = match two {
            Some(['$', '$']) => Some(&['$', '$']),
            Some(['\\', '[']) => Some(&['\\', ']']),
            Some(['\\', '(']) => Some(&['\\', ')']),
            _ => None,
        };
        if let Some(close) = close
            && let Some(end) = find(i + 2, close)
        {
            spans.push((i, end + 2));
            i = end + 2;
            continue;
        }
        if chars[i] == '$'
            && chars.get(i + 1).is_some_and(|c| !c.is_whitespace())
            && let Some(end) = (i + 1..chars.len()).find(|&k| chars[k] == '$')
            && !chars[end - 1].is_whitespace()
            && !chars.get(end + 1).is_some_and(char::is_ascii_digit)
        {
            spans.push((i, end + 1));
            i = end + 1;
            continue;
        }
        i += 1;
    }
    spans
}

/// `&` 后面像 HTML 实体（`&amp;`、`&#123;`、`&#x1F;`）。
fn entity_like(rest: &[char]) -> bool {
    let semi = rest.iter().take(34).position(|&c| c == ';');
    let Some(end) = semi else { return false };
    let body = &rest[..end];
    match body {
        ['#', 'x' | 'X', hex @ ..] => {
            (1..=6).contains(&hex.len()) && hex.iter().all(char::is_ascii_hexdigit)
        }
        ['#', dec @ ..] => (1..=7).contains(&dec.len()) && dec.iter().all(char::is_ascii_digit),
        [first, more @ ..] => {
            first.is_ascii_alphabetic()
                && !more.is_empty()
                && more.iter().all(char::is_ascii_alphanumeric)
        }
        [] => false,
    }
}

/// 段落里一行的开头像块级标记时转义（行从 `start` 开始，已经折叠成单行）。
fn escape_block_start(text: &mut String, start: usize) {
    let line = &text[start..];
    let b = line.as_bytes();
    let only = |set: &[u8]| b.iter().all(|c| set.contains(c) || *c == b' ');
    let thematic = |c: u8| only(&[c]) && b.iter().filter(|&&x| x == c).count() >= 3;
    let at = match b.first() {
        Some(b'#') => {
            let n = b.iter().take_while(|&&c| c == b'#').count();
            (n <= 6 && matches!(b.get(n), None | Some(b' '))).then_some(0)
        }
        Some(b'-' | b'+' | b'*') => {
            (matches!(b.get(1), None | Some(b' ')) || thematic(b[0]) || only(b"-")).then_some(0)
        }
        Some(b'_') => thematic(b'_').then_some(0),
        Some(b'=') => only(b"=").then_some(0),
        Some(b'>') => Some(0),
        Some(b'~') => line.starts_with("~~~").then_some(0),
        Some(b'|') => None,
        Some(c) if c.is_ascii_digit() => {
            let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            (n <= 9
                && matches!(b.get(n), Some(b'.' | b')'))
                && matches!(b.get(n + 1), None | Some(b' ')))
            .then_some(n)
        }
        _ => None,
    };
    if let Some(i) = at {
        text.insert(start + i, '\\');
    }
}

/// 表格单元格里不会出现列表、标题、引用，段落开头为此加的转义去掉。
fn unescape_block_start(cell: &str) -> String {
    let b = cell.as_bytes();
    if b.first() == Some(&b'\\')
        && matches!(
            b.get(1),
            Some(b'#' | b'-' | b'+' | b'*' | b'>' | b'=' | b'_' | b'~')
        )
    {
        return cell[1..].to_string();
    }
    let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if n > 0 && b.get(n) == Some(&b'\\') && matches!(b.get(n + 1), Some(b'.' | b')')) {
        return format!("{}{}", &cell[..n], &cell[n + 1..]);
    }
    cell.to_string()
}

fn join_blocks(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 正文一级标题之前的站点框架：logo、站点名链接、"前往官网"、搜索框提示这类块，
/// 常常既不在 `<header>` 也不在 `<nav>` 里，跳过规则抓不到。只取正文时，如果前十几个块里有
/// 一级标题，就去掉它前面"像框架"的块（纯链接、纯图片、或很短又不成句）；标题块和成句的段落保留。
/// 评测集上噪声泄漏降 12%，召回基本不变。
fn trim_leading_chrome(blocks: &mut Vec<Block>) {
    let Some(h1) = blocks
        .iter()
        .take(12)
        .position(|b| b.text.starts_with("# "))
    else {
        return;
    };
    let mut i = 0;
    blocks.retain(|b| {
        let keep = i >= h1 || !looks_like_chrome(&b.text);
        i += 1;
        keep
    });
}

fn looks_like_chrome(text: &str) -> bool {
    // 标题是内容的组织结构，不当框架删
    if text.starts_with('#') {
        return false;
    }
    let plain = strip_links(&strip_images(text));
    let words = plain.split_whitespace().count();
    if words == 0 {
        return true;
    }
    let sentence = plain.contains(['.', '。', '!', '?', '！', '？']);
    !sentence && (words <= 4 || plain.chars().count() < 40)
}

/// 去掉 `![alt](src)` 图片。
fn strip_images(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        rest = after.find(')').map_or("", |j| &after[j + 1..]);
    }
    out.push_str(rest);
    out
}

/// 去掉 `[text](href)` 链接（连同链接文字），纯链接的块因此变空。
fn strip_links(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        match (after.find("]("), after.find(')')) {
            (Some(_), Some(j)) => rest = &after[j + 1..],
            _ => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}
