//! 各格式共用的 Markdown 输出：先收集成块，最后统一渲染（GFM）。

/// 一个 Markdown 块。行内文字由调用方用 [`escape`] 处理好（行内的加粗、链接等记号已经拼进去）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    Heading(u8, String),
    Para(String),
    /// 列表项：嵌套层级（0 起）、是否有序、编号、文字。
    Item {
        level: u8,
        ordered: bool,
        number: u32,
        text: String,
    },
    /// 第一行当表头。单元格是已经转义好的行内文字。
    Table(Vec<Vec<String>>),
    /// 原样输出（例如 `<!-- slide 3 -->`、脚注定义）。
    Raw(String),
}

/// 渲染成 Markdown：块之间空一行，相邻的列表项之间不空行。
pub(crate) fn render(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut prev_item = false;
    for b in blocks {
        let is_item = matches!(b, Block::Item { .. });
        if !out.is_empty() {
            out.push_str(if is_item && prev_item { "\n" } else { "\n\n" });
        }
        prev_item = is_item;
        match b {
            Block::Heading(level, text) => {
                out.push_str(&"#".repeat(usize::from((*level).clamp(1, 6))));
                out.push(' ');
                out.push_str(text);
            }
            Block::Para(text) => out.push_str(&guard_lines(text, 0)),
            Block::Item {
                level,
                ordered,
                number,
                text,
            } => {
                // 有序列表的子项要缩进到父项文字对齐的位置；统一按 4 个空格一级，两种列表都能嵌套
                let indent = "    ".repeat(usize::from(*level));
                let marker = if *ordered {
                    format!("{number}. ")
                } else {
                    "- ".to_string()
                };
                out.push_str(&indent);
                out.push_str(&marker);
                // 段内换行之后的续行缩进到列表项文字的位置，仍属于这一项
                out.push_str(&guard_lines(text, indent.len() + marker.len()));
            }
            Block::Table(rows) => table(&mut out, rows),
            Block::Raw(s) => out.push_str(s),
        }
    }
    out.push('\n');
    out
}

fn table(out: &mut String, rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for c in 0..cols {
            out.push(' ');
            out.push_str(&cell(row.get(c).map_or("", String::as_str)));
            out.push_str(" |");
        }
        if i == 0 {
            out.push_str("\n|");
            out.push_str(&" --- |".repeat(cols));
        }
        if i + 1 < rows.len() {
            out.push('\n');
        }
    }
}

/// 表格单元格：竖线转义，换行变 `<br>`，空白合一。
fn cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s
        .split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .enumerate()
    {
        if i > 0 {
            out.push_str("<br>");
        }
        let mut space = false;
        for ch in line.chars() {
            if ch.is_whitespace() {
                space = true;
                continue;
            }
            if space && !out.is_empty() && !out.ends_with("<br>") {
                out.push(' ');
            }
            space = false;
            if ch == '|' {
                out.push('\\');
            }
            out.push(ch);
        }
    }
    out
}

/// 行内文字转义：会被当成 Markdown 记号的字符前面加反斜杠。
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 段内每一行（硬换行之后的续行也会被当成新块的开头）都做 [`guard_line_start`]，续行前补 `indent` 个空格。
fn guard_lines(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(&pad);
        }
        out.push_str(&guard_line_start(line));
    }
    out
}

/// 段落开头像块记号（标题、引用、列表、分隔线）时转义第一个字符，免得被当成别的块。
fn guard_line_start(text: &str) -> String {
    let t = text.trim_start();
    let needs = t.starts_with('#')
        || t.starts_with('>')
        || t.starts_with("- ")
        || t.starts_with("+ ")
        || t.starts_with("* ")
        || t.starts_with("===")
        || t.starts_with("---")
        || {
            let digits = t.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0
                && digits < 10
                && matches!(t.as_bytes().get(digits..digits + 2), Some(b". " | b") "))
        };
    if !needs {
        return t.to_string();
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        format!("{}\\{}", &t[..digits], &t[digits..])
    } else {
        format!("\\{t}")
    }
}

/// 把连续空白合成一个空格并去掉首尾空白（`\n` 保留，用于单元格和段内换行）。
pub(crate) fn squeeze(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for ch in s.chars() {
        if ch == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
            space = false;
        } else if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        }
    }
    out.trim_matches(|c: char| c.is_whitespace()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_blocks_lists_and_tables() {
        let md = render(&[
            Block::Heading(2, "Title".into()),
            Block::Para("# not a heading".into()),
            Block::Item {
                level: 0,
                ordered: true,
                number: 3,
                text: "three".into(),
            },
            Block::Item {
                level: 1,
                ordered: false,
                number: 0,
                text: "sub".into(),
            },
            Block::Item {
                level: 0,
                ordered: false,
                number: 0,
                text: "first\\\n# second".into(),
            },
            Block::Para("line\\\n- not a list".into()),
            Block::Table(vec![
                vec!["a".into(), "b|c".into()],
                vec!["line1\nline2".into()],
            ]),
        ]);
        assert_eq!(
            md,
            "## Title\n\n\\# not a heading\n\n3. three\n    - sub\n- first\\\n  \\# second\n\nline\\\n\\- not a list\n\n| a | b\\|c |\n| --- | --- |\n| line1<br>line2 |  |\n"
        );
    }

    #[test]
    fn escapes_markdown_syntax() {
        assert_eq!(escape("a*b_c [x] <y>"), "a\\*b\\_c \\[x\\] \\<y\\>");
        assert_eq!(guard_line_start("1. item"), "1\\. item");
        assert_eq!(guard_line_start("2026 was"), "2026 was");
        assert_eq!(squeeze("  a \t b \n  c  "), "a b\nc");
    }
}
