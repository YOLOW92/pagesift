//! DOM 子树转 GFM。分两层：块级渲染产出段落列表，行内渲染产出单行文本。

use pagesift_html::{Element, Tag};
use url::Url;

use crate::dom::{NodeR, collapse_whitespace, collapse_whitespace_into, element, raw_text, tag};
use crate::extract::Skipper;

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

    fn skip(&self, node: &NodeR<'_>) -> bool {
        node.is_element() && self.skipper.skip(node) || node.is_comment()
    }

    /// 元素按块处理还是按行内处理。未知元素（自定义元素等）看里面有没有块级内容。
    fn acts_as_block(&self, node: &NodeR<'_>) -> bool {
        match node.tag() {
            Some(t) if is_block_tag(t) => true,
            Some(t) if is_inline_tag(t) => self.skipper.has_block_descendant(node),
            Some(_) => self.skipper.has_block_descendant(node),
            None => false,
        }
    }

    /// 容器的子节点：连续的行内内容合成一个段落，遇到块级子元素就断开。
    fn blocks_of(&self, node: NodeR<'_>) -> Vec<Block> {
        let mut blocks = Vec::new();
        let mut inline = String::new();
        for child in node.children() {
            if self.skip(&child) {
                continue;
            }
            if child.is_element() && self.acts_as_block(&child) {
                flush_paragraph(&mut inline, &mut blocks);
                self.block(child, &mut blocks);
            } else {
                self.inline_into(child, &mut inline);
            }
        }
        flush_paragraph(&mut inline, &mut blocks);
        blocks
    }

    fn block(&self, node: NodeR<'_>, out: &mut Vec<Block>) {
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
                    .map(|c| single_line(&join_blocks(&self.blocks_of(c))).replace('|', "\\|"))
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
            collapse_whitespace_into(t, out);
            return;
        }
        let Some(el) = element(&node) else {
            return;
        };
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
        let src = el
            .attr("src")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| el.attr("data-src"))
            .map(str::trim);
        let Some(src) = src else {
            return String::new();
        };
        if src.starts_with("data:") {
            return String::new();
        }
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

fn flush_paragraph(buf: &mut String, blocks: &mut Vec<Block>) {
    let mut text = String::new();
    for line in buf.split('\n') {
        if line.split_whitespace().next().is_none() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        push_single_line(line, &mut text);
    }
    buf.clear();
    if !text.is_empty() {
        blocks.push(Block {
            kind: Kind::Para,
            text,
        });
    }
}

fn join_blocks(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}
