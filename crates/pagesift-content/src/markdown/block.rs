//! 块级渲染：段落、代码块、列表、表格。

use super::*;

impl Renderer<'_> {
    /// 容器的子节点：连续的行内内容合成一个段落，遇到块级子元素就断开。
    /// 白名单模式下，不在允许元素里的行内文字丢掉。
    pub(super) fn blocks_of(&self, node: NodeR<'_>) -> Vec<Block> {
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

    pub(super) fn block(&self, node: NodeR<'_>, out: &mut Vec<Block>) {
        if let Some(m) = self.math(&node) {
            if let Math::Tex(tex, _) = m {
                out.push(Block {
                    kind: Kind::Other,
                    text: format!("$$\n{tex}\n$$"),
                });
            }
            return;
        }
        // GitHub 的文件视图把行号和代码分成两列：行号列是一串独立数字，代码列每行一个
        // `div`。行号列整块丢掉，代码列合并进一个围栏，否则行号会堆在正文前面。
        if class_contains(&node, "react-line-numbers") {
            return;
        }
        if class_contains(&node, "react-code-lines")
            && let Some(block) = self.merged_code_lines(node)
        {
            out.push(block);
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
                let (blocks, indent) = self.nested(|| self.blocks_of(node));
                // 超过嵌套上限：不再加 `>`，块原样交给外层（不拼接、不复制）
                if !indent {
                    out.extend(blocks);
                    return;
                }
                let inner = join_blocks(&blocks);
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
            // 已经在表格内部：内层表格不再展开成子表，按普通容器透传文字。
            // 排版型页面常见几十层嵌套 table，逐层展开会让单元格重复渲染，代价按深度指数增长。
            "table" if self.flat => out.extend(self.blocks_of(node)),
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

    /// 把代码视图里"一行一个 `div`"的代码拼回一个围栏块。GitHub 的行号列是兄弟节点，
    /// 已经在 [`Renderer::block`] 里整块丢掉了。
    pub(super) fn merged_code_lines(&self, node: NodeR<'_>) -> Option<Block> {
        let mut lines = Vec::new();
        for line in node.descendants() {
            let Some(el) = element(&line) else { continue };
            if class_contains_el(el, "react-file-line") {
                // 行内 span 之间的排版换行不是代码换行；去掉，免得一行变两行。
                lines.push(raw_text(&line).replace(['\n', '\r'], ""));
            }
        }
        if lines.is_empty() {
            return None;
        }
        let text = lines.join("\n");
        let text = text.trim_end_matches(['\n', '\r']);
        if text.trim().is_empty() {
            return None;
        }
        let mut fence = "```".to_string();
        while text.contains(&fence) {
            fence.push('`');
        }
        Some(Block {
            kind: Kind::Other,
            text: format!("{fence}\n{text}\n{fence}"),
        })
    }

    pub(super) fn code_block(&self, node: NodeR<'_>) -> Option<Block> {
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

    pub(super) fn list(&self, node: NodeR<'_>, ordered: bool) -> Option<Block> {
        let mut n: i64 = element(&node)
            .and_then(|e| e.attr("start"))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let mut items = Vec::new();
        let mut loose = false;
        // 超过嵌套上限的层：续行不再缩进
        let mut deep = false;
        for child in node.children() {
            if self.skip(&child) {
                continue;
            }
            let (blocks, indent_ok) = self.nested(|| self.list_item(child));
            if !indent_ok {
                deep = true;
            }
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
            // 超过嵌套上限：续行不缩进，内容原样接在标记后面
            if deep {
                items.push(marker + &content);
                continue;
            }
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

    /// 列表里的一个子节点：`<li>` 的内容；直接放在 ul/ol 里的非 li 元素按内容处理，不丢文本。
    fn list_item(&self, child: NodeR<'_>) -> Vec<Block> {
        if tag(&child) == Some("li") {
            return self.blocks_of(child);
        }
        let mut v = Vec::new();
        if !child.is_element() {
            return v;
        }
        if self.acts_as_block(&child) {
            self.block(child, &mut v);
        } else {
            let mut s = String::new();
            self.inline_into(child, &mut s);
            flush_paragraph(&mut s, &mut v);
        }
        v
    }

    pub(super) fn table(&self, node: NodeR<'_>, out: &mut Vec<Block>) {
        let mut rows: Vec<NodeR<'_>> = Vec::new();
        collect_rows(node, &mut rows);
        // 单元格内部按"扁平"方式渲染：内层 table 只透传文字，不再展开成子表。
        // 排版型页面常出现几十层嵌套 table（真实样本最深 27 层、241 个 table），
        // 若每层都完整展开，外层单元格会把内层子树重复渲染一次，代价随深度指数增长。
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                row.children()
                    .filter(|c| matches!(tag(c), Some("td" | "th")))
                    .map(|c| {
                        let cell = single_line(
                            &join_blocks(&self.as_flat().blocks_of(c)).replace("  \n", " "),
                        );
                        unescape_block_start(&cell).replace('|', "\\|")
                    })
                    .collect()
            })
            .filter(|r: &Vec<String>| !r.is_empty())
            .collect();
        let cols = cells.iter().map(Vec::len).max().unwrap_or(0);
        // 单行或单列的表格基本是排版用的，不当数据表
        if cells.len() < 2 || cols < 2 {
            out.extend(self.as_flat().blocks_of(node));
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
}

pub(super) fn collect_rows<'a>(node: NodeR<'a>, rows: &mut Vec<NodeR<'a>>) {
    for child in node.children() {
        match tag(&child) {
            Some("tr") => rows.push(child),
            Some("table") => {}
            Some(_) => collect_rows(child, rows),
            None => {}
        }
    }
}

pub(super) fn code_language(pre: &NodeR<'_>) -> Option<String> {
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

pub(super) fn is_lang_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "+-_#.".contains(ch))
}

/// 一段行内内容收成段落。行内的换行来自 `<br>`，写成硬换行（行尾两个空格，看不见，不添噪声）；
/// 每行开头像块级标记（`# `、`- `、`1. `、`> `…）的转义掉，免得被当成标题、列表、引用。
pub(super) fn flush_paragraph(buf: &mut String, blocks: &mut Vec<Block>) {
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
