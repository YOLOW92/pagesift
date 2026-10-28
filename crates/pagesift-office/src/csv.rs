//! CSV（RFC 4180）转 Markdown 表格，第一行当表头。
//!
//! 照 RFC 4180 解析引号字段（`""` 是一个引号，引号里可以有分隔符和换行），行尾 CRLF、LF、CR 都认；
//! 不规范的写法（未加引号的字段里出现引号）按字面保留，不报错。编码：有 BOM 按 BOM，
//! 合法 UTF-8 按 UTF-8，否则按 Windows-1252；知道字符集（HTTP 头、探测器）的调用方先自己解码，
//! 再走 [`convert_text`]。分隔符在逗号、分号、制表符、竖线里按前几十行判断。

use crate::md::{Grid, escape_into};
use crate::{Converted, Error, Options, check_deadline};

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    convert_text(&decode(bytes), opts)
}

/// 已经解码成 UTF-8 的 CSV。
pub(crate) fn convert_text(text: &str, opts: &Options<'_>) -> Result<Converted, Error> {
    let delim = sniff(text);
    let mut out = Converted::default();
    let max_cols = opts.limits.max_columns;
    let mut budget = opts.limits.max_cells;
    // 整张表连续存放（见 `md::Grid`）；一条记录的字段读进同一个复用的缓冲区，不按格分配
    let mut grid = Grid::default();
    let mut fields = String::new();
    let mut ends: Vec<usize> = Vec::new();
    let mut escaped = String::new();
    let mut records = Records::new(text, delim);
    let mut wide = false;
    let mut unbilled = 0usize;
    // 末尾连续几行全是空字段（文件末尾的空行，不输出）
    let mut empty_tail = 0usize;
    let mut i = 0usize;
    while records.next_fields(&mut fields, &mut ends) {
        if i % 4096 == 4095 {
            check_deadline(opts)?;
        }
        i += 1;
        // 输出预算（见 `budget` 模块）：一个字段可以有几十 MB
        unbilled += fields.len();
        if unbilled >= crate::budget::BATCH && !crate::budget::spend(std::mem::take(&mut unbilled))
        {
            break;
        }
        let mut n = ends.len();
        if n > max_cols {
            n = max_cols;
            wide = true;
        }
        if n > budget {
            out.warnings.push(format!(
                "CSV truncated at the cell limit ({} cells)",
                opts.limits.max_cells
            ));
            break;
        }
        budget -= n;
        let mut start = 0;
        let mut empty = true;
        for &end in &ends[..n] {
            let field = &fields[start..end];
            start = end;
            empty &= field.is_empty();
            escaped.clear();
            escape_into(&mut escaped, field);
            grid.push_cell(&escaped);
        }
        grid.end_row();
        empty_tail = if empty { empty_tail + 1 } else { 0 };
    }
    grid.truncate_rows(grid.rows() - empty_tail);
    if wide {
        out.warnings
            .push(format!("columns beyond {max_cols} were dropped"));
    }
    out.parts = grid.rows();
    out.markdown = if grid.rows() == 0 {
        String::new()
    } else {
        grid.render()
    };
    Ok(out)
}

fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, ..] | [0xFE, 0xFF, ..] => crate::xml::decode(bytes.to_vec()),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
        },
    }
}

/// Windows-1252（WHATWG Encoding 标准的表）：0x80–0x9F 是印刷符号，其余字节和 Latin-1 相同；
/// 标准里没定义的 0x81、0x8D、0x8F、0x90、0x9D 按同值的 C1 控制字符。
fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

/// 在前 50 条记录里，哪个候选分隔符让每行字段数最一致（且多于 1 列）。都不像时用逗号。
fn sniff(text: &str) -> char {
    let mut best = (',', 0usize, 0usize);
    for d in [',', ';', '\t', '|'] {
        let mut records = Records::new(text, d);
        let (mut fields, mut ends) = (String::new(), Vec::new());
        let mut counts: Vec<usize> = Vec::new();
        for _ in 0..50 {
            if !records.next_fields(&mut fields, &mut ends) {
                break;
            }
            counts.push(ends.len());
        }
        if counts.is_empty() {
            continue;
        }
        let mode = mode(&counts);
        if mode < 2 {
            continue;
        }
        let agree = counts.iter().filter(|&&c| c == mode).count();
        // 一致的行多者胜；一样多时列多者胜
        if (agree, mode) > (best.1, best.2) {
            best = (d, agree, mode);
        }
    }
    best.0
}

fn mode(v: &[usize]) -> usize {
    let mut sorted = v.to_vec();
    sorted.sort_unstable();
    let mut best = (0, 0);
    let mut i = 0;
    while i < sorted.len() {
        let j = sorted[i..].iter().take_while(|&&x| x == sorted[i]).count();
        if j > best.1 {
            best = (sorted[i], j);
        }
        i += j;
    }
    best.0
}

/// 按 RFC 4180 逐条读记录。
struct Records<'a> {
    s: &'a str,
    pos: usize,
    delim: char,
}

impl<'a> Records<'a> {
    fn new(s: &'a str, delim: char) -> Self {
        Records { s, pos: 0, delim }
    }
}

impl Records<'_> {
    /// 读下一条记录：各字段依次写进 `fields`（先清空），`ends` 记每个字段在其中的结束位置。
    /// 没有记录了返回 false。
    fn next_fields(&mut self, fields: &mut String, ends: &mut Vec<usize>) -> bool {
        fields.clear();
        ends.clear();
        if self.pos >= self.s.len() {
            return false;
        }
        let field = fields;
        let mut chars = self.s[self.pos..].char_indices().peekable();
        let mut quoted = false;
        let mut at_start = true;
        let mut consumed = self.s.len() - self.pos;
        while let Some((i, c)) = chars.next() {
            if quoted {
                if c == '"' {
                    if chars.peek().is_some_and(|(_, n)| *n == '"') {
                        field.push('"');
                        chars.next();
                    } else {
                        quoted = false;
                    }
                } else {
                    field.push(c);
                }
                continue;
            }
            match c {
                '"' if at_start => {
                    quoted = true;
                    at_start = false;
                }
                c if c == self.delim => {
                    ends.push(field.len());
                    at_start = true;
                }
                '\r' | '\n' => {
                    let mut end = i + 1;
                    if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
                        end += 1;
                    }
                    consumed = end;
                    break;
                }
                c => {
                    field.push(c);
                    at_start = false;
                }
            }
        }
        ends.push(field.len());
        self.pos += consumed;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(s: &[u8]) -> String {
        convert(s, &Options::default()).unwrap().markdown
    }

    #[test]
    fn rfc4180_quotes_newlines_and_line_endings() {
        assert_eq!(
            md(b"a,b\r\n\"x, y\",\"he said \"\"hi\"\"\"\r\n\"multi\nline\",3\r\n"),
            "| a | b |\n| --- | --- |\n| x, y | he said \"hi\" |\n| multi<br>line | 3 |\n"
        );
        assert_eq!(md(b"a,b\rc,d\r"), "| a | b |\n| --- | --- |\n| c | d |\n");
        assert_eq!(
            md(b"a,b\n1\n2,3,4\n"),
            "| a | b |  |\n| --- | --- | --- |\n| 1 |  |  |\n| 2 | 3 | 4 |\n"
        );
        assert_eq!(md(b""), "");
    }

    #[test]
    fn sniffs_delimiters_and_encodings() {
        assert_eq!(
            md(b"name;price\nTea;1,50\nCake;2,00\n"),
            "| name | price |\n| --- | --- |\n| Tea | 1,50 |\n| Cake | 2,00 |\n"
        );
        assert_eq!(md(b"a\tb\n1\t2\n"), "| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_eq!(
            md(b"\xEF\xBB\xBFcaf\xC3\xA9,x\n"),
            "| café | x |\n| --- | --- |\n"
        );
        assert_eq!(md(b"caf\xE9,x\n"), "| café | x |\n| --- | --- |\n");
        // Windows-1252 的 0x80–0x9F：弯引号、欧元符号；没定义的 0x81 按 C1 控制字符
        assert_eq!(
            md(b"\x93q\x94 \x80\x81,x\n"),
            "| \u{201C}q\u{201D} \u{20AC}\u{81} | x |\n| --- | --- |\n"
        );
        assert_eq!(
            md(b"a|b,c\n1|2,3\n"),
            "| a\\|b | c |\n| --- | --- |\n| 1\\|2 | 3 |\n"
        );
    }

    #[test]
    fn huge_csvs_are_capped() {
        let big: String = (0..1000).map(|i| format!("{i},{i}\n")).collect();
        let opts = Options {
            limits: crate::Limits {
                max_cells: 100,
                ..Default::default()
            },
            ..Default::default()
        };
        let got = convert(big.as_bytes(), &opts).unwrap();
        assert_eq!(got.parts, 50);
        assert!(!got.warnings.is_empty());
    }

    #[test]
    fn the_rendered_table_is_capped_even_when_the_cells_fit() {
        // 3 万行一个字节的单元格：文字 30 KB，渲染成表格是 180 KB
        let csv = "h\n".to_string() + &"x\n".repeat(30_000);
        let opts = Options {
            limits: crate::Limits {
                max_output_bytes: 64 << 10,
                ..crate::Limits::default()
            },
            ..Options::default()
        };
        let got = crate::convert(csv.as_bytes(), crate::Format::Csv, &opts).unwrap();
        assert!(got.markdown.len() <= 64 << 10, "{}", got.markdown.len());
        assert!(got.markdown.ends_with("| x |\n"), "cut at a row boundary");
        assert!(
            got.warnings
                .iter()
                .any(|w| w.starts_with("output_truncated:")),
            "{:?}",
            got.warnings
        );
    }

    #[test]
    fn a_huge_field_stops_the_conversion_at_the_output_budget() {
        let mut csv = b"a,b\n1,2\n3,\"".to_vec();
        csv.extend(std::iter::repeat_n(b'x', 1 << 20));
        csv.extend(b"\"\n5,6\n");
        let opts = Options {
            limits: crate::Limits {
                max_output_bytes: 256 << 10,
                ..crate::Limits::default()
            },
            ..Options::default()
        };
        let got = crate::convert(&csv, crate::Format::Csv, &opts).unwrap();
        assert!(
            got.markdown.starts_with("| a | b |"),
            "{}",
            &got.markdown[..40]
        );
        assert!(!got.markdown.contains("| 5 | 6 |"));
        assert!(
            got.warnings
                .iter()
                .any(|w| w.starts_with("output_truncated:")),
            "{:?}",
            got.warnings
        );
    }
}
