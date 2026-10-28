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
    cell_into(&mut out, s);
    out
}

/// 表格单元格的文字：各行去掉首尾空白、跳过空行，行之间用 `<br>`；行内的空白压成一个空格，
/// `|` 转义。追加到 `out` 末尾。按字节成段处理：普通字节整段拷贝，只有 ASCII 空白、`|`、换行和
/// 可能是 Unicode 空白的首字节（见 [`maybe_space`]）才逐个看。
fn cell_into(out: &mut String, s: &str) {
    let start = out.len();
    let b = s.as_bytes();
    // 这一格已经输出过字符；这一行已经输出过字符；上一个输出的字符后面有空白
    let (mut any, mut line, mut space) = (false, false, false);
    let mut i = 0;
    while i < b.len() {
        // 普通字节的一段
        let run = i;
        while i < b.len() && !maybe_space(b[i]) && b[i] != b'|' {
            i += 1;
        }
        if i > run {
            emit_lead(out, start, &mut any, &mut line, &mut space);
            out.push_str(&s[run..i]);
        }
        if i == b.len() {
            break;
        }
        match b[i] {
            b'\n' => {
                line = false;
                space = false;
                i += 1;
            }
            b'|' => {
                emit_lead(out, start, &mut any, &mut line, &mut space);
                out.push_str("\\|");
                i += 1;
            }
            c if c < 0x80 => {
                // ASCII 空白：行首的不算（行要去掉首尾空白）
                space |= line;
                i += 1;
            }
            _ => {
                let ch = s[i..].chars().next().expect("char boundary");
                let n = ch.len_utf8();
                if ch.is_whitespace() {
                    space |= line;
                } else {
                    emit_lead(out, start, &mut any, &mut line, &mut space);
                    out.push_str(&s[i..i + n]);
                }
                i += n;
            }
        }
    }
}

/// 输出一个非空白字符之前：新的一行（前面有过内容）先补 `<br>`，行内空白补一个空格
/// （这一格的文字正以 `<br>` 结尾时不补）。`start` 是这一格在 `out` 里的起点。
fn emit_lead(out: &mut String, start: usize, any: &mut bool, line: &mut bool, space: &mut bool) {
    if !*line && *any {
        out.push_str("<br>");
    } else if *space && !out[start..].ends_with("<br>") {
        out.push(' ');
    }
    *any = true;
    *line = true;
    *space = false;
}

/// 这个字节可能是空白的开头：ASCII 空白，或 Unicode 空白（U+0085、U+00A0、U+1680、
/// U+2000–U+205F、U+3000）的 UTF-8 首字节。
fn maybe_space(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | 0xC2 | 0xE1 | 0xE2 | 0xE3
    )
}

/// 行内文字转义：会被当成 Markdown 记号的字符前面加反斜杠。
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    escape_into(&mut out, s);
    out
}

/// [`escape`]，追加到 `out` 末尾。
pub(crate) fn escape_into(out: &mut String, s: &str) {
    // 要转义的都是 ASCII：按字节找，中间整段拷贝
    let mut run = 0;
    for (i, &c) in s.as_bytes().iter().enumerate() {
        if matches!(c, b'\\' | b'`' | b'*' | b'_' | b'[' | b']' | b'<' | b'>') {
            out.push_str(&s[run..i]);
            out.push('\\');
            run = i;
        }
    }
    out.push_str(&s[run..]);
}

/// 连续存放的表格：各单元格整理好的文字按行顺序写在同一个缓冲区里，另记每格、每行的结束位置。
/// 几十万格的 CSV 存成 `Vec<Vec<String>>` 时每格单独分配、散在堆上，读写都跳着走
/// （cachegrind：末级缓存未命中占 CSV 转换估算周期的约 15%，八成出在分配和拷贝上）。
#[derive(Debug, Default)]
pub(crate) struct Grid {
    text: String,
    cell_ends: Vec<usize>,
    row_ends: Vec<usize>,
}

impl Grid {
    /// 当前行追加一格；`escaped` 是已经 [`escape`] 过的行内文字，这里再按单元格整理。
    pub(crate) fn push_cell(&mut self, escaped: &str) {
        cell_into(&mut self.text, escaped);
        self.cell_ends.push(self.text.len());
    }

    /// 结束当前行。
    pub(crate) fn end_row(&mut self) {
        self.row_ends.push(self.cell_ends.len());
    }

    pub(crate) fn rows(&self) -> usize {
        self.row_ends.len()
    }

    /// 只保留前 `n` 行。
    pub(crate) fn truncate_rows(&mut self, n: usize) {
        if n >= self.row_ends.len() {
            return;
        }
        self.row_ends.truncate(n);
        let cells = self.row_ends.last().copied().unwrap_or(0);
        self.cell_ends.truncate(cells);
        self.text
            .truncate(self.cell_ends.last().copied().unwrap_or(0));
    }

    /// 第 `r` 行的单元格。
    fn row(&self, r: usize) -> impl Iterator<Item = &str> {
        let first = if r == 0 { 0 } else { self.row_ends[r - 1] };
        let start = if first == 0 {
            0
        } else {
            self.cell_ends[first - 1]
        };
        self.cell_ends[first..self.row_ends[r]]
            .iter()
            .scan(start, |at, &end| {
                let s = &self.text[*at..end];
                *at = end;
                Some(s)
            })
    }

    /// 渲染成 Markdown 表格（和 `render(&[Block::Table(..)])` 输出相同），第一行当表头。
    pub(crate) fn render(&self) -> String {
        let rows = self.rows();
        let cols = (0..rows)
            .map(|r| {
                let first = if r == 0 { 0 } else { self.row_ends[r - 1] };
                self.row_ends[r] - first
            })
            .max()
            .unwrap_or(0)
            .max(1);
        // 每格多出 `| ` 和空格，表头下面一行分隔线
        let mut out = String::with_capacity(self.text.len() + (rows + 1) * (cols * 6 + 2) + 1);
        for r in 0..rows {
            out.push('|');
            let mut n = 0;
            for c in self.row(r) {
                out.push(' ');
                out.push_str(c);
                out.push_str(" |");
                n += 1;
            }
            for _ in n..cols {
                out.push_str("  |");
            }
            if r == 0 {
                out.push_str("\n|");
                out.push_str(&" --- |".repeat(cols));
            }
            if r + 1 < rows {
                out.push('\n');
            }
        }
        out.push('\n');
        out
    }
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
pub(crate) mod tests {
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

    /// 旧的逐字符实现，作为成段处理版本的参照。
    fn cell_reference(s: &str) -> String {
        let mut out = String::new();
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

    fn escape_reference(s: &str) -> String {
        let mut out = String::new();
        for ch in s.chars() {
            if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>') {
                out.push('\\');
            }
            out.push(ch);
        }
        out
    }

    /// 由片段随机拼成的字符串（xorshift，固定种子）：各种空白、换行、`|`、字面的 `<br>`、多字节字符。
    pub(crate) fn tricky_strings(n: usize) -> Vec<String> {
        const ATOMS: &[&str] = &[
            "a", "Bc", " ", "  ", "\t", "\n", "\r", "\r\n", "|", "<br>", "<br> ", "\u{a0}",
            "\u{3000}", "\u{2028}", "\u{85}", "\u{1680}", "\u{205f}", "\u{200b}", "\u{2009}", "é",
            "中文", "Ÿ", "\u{e2}", "*_`", "[x]", "\\", "\"", "\"\"", ",", ";", "x,y", "\u{feff}",
            "😀",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        (0..n)
            .map(|_| {
                let len = (next() % 12) as usize;
                (0..len)
                    .map(|_| ATOMS[(next() % ATOMS.len() as u64) as usize])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn byte_run_versions_match_the_char_by_char_ones() {
        for s in tricky_strings(20_000) {
            assert_eq!(cell(&s), cell_reference(&s), "{s:?}");
            assert_eq!(escape(&s), escape_reference(&s), "{s:?}");
            // 接在别的单元格后面写（共用缓冲区）结果也一样
            let mut out = String::from("x<br>");
            cell_into(&mut out, &s);
            assert_eq!(out, format!("x<br>{}", cell_reference(&s)), "{s:?}");
        }
    }

    #[test]
    fn grid_renders_like_a_table_block() {
        let rows = vec![
            vec!["a".to_string(), "b|c".into(), "x  y".into()],
            vec!["line1\nline2".into()],
            vec![],
            vec!["".into(), " ".into()],
        ];
        let mut g = Grid::default();
        for row in &rows {
            for c in row {
                g.push_cell(c);
            }
            g.end_row();
        }
        assert_eq!(g.render(), render(&[Block::Table(rows.clone())]));
        // 只留前两行
        g.truncate_rows(2);
        assert_eq!(g.render(), render(&[Block::Table(rows[..2].to_vec())]));
        assert_eq!(g.rows(), 2);
    }

    #[test]
    fn escapes_markdown_syntax() {
        assert_eq!(escape("a*b_c [x] <y>"), "a\\*b\\_c \\[x\\] \\<y\\>");
        assert_eq!(guard_line_start("1. item"), "1\\. item");
        assert_eq!(guard_line_start("2026 was"), "2026 was");
        assert_eq!(squeeze("  a \t b \n  c  "), "a b\nc");
    }
}
