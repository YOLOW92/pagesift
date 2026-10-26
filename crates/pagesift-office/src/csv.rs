//! CSV（RFC 4180）转 Markdown 表格，第一行当表头。
//!
//! 照 RFC 4180 解析引号字段（`""` 是一个引号，引号里可以有分隔符和换行），行尾 CRLF、LF、CR 都认；
//! 不规范的写法（未加引号的字段里出现引号）按字面保留，不报错。编码：有 BOM 按 BOM，
//! 合法 UTF-8 按 UTF-8，否则按 Windows-1252。分隔符在逗号、分号、制表符、竖线里按前几十行判断。

use crate::md::{Block, escape, render};
use crate::{Converted, Error, Options, check_deadline};

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let text = decode(bytes);
    let delim = sniff(&text);
    let mut out = Converted::default();
    let max_cols = opts.limits.max_columns;
    let mut budget = opts.limits.max_cells;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut wide = false;
    for (i, mut row) in Records::new(&text, delim).enumerate() {
        if i % 4096 == 4095 {
            check_deadline(opts)?;
        }
        if row.len() > max_cols {
            row.truncate(max_cols);
            wide = true;
        }
        if row.len() > budget {
            out.warnings.push(format!(
                "CSV truncated at the cell limit ({} cells)",
                opts.limits.max_cells
            ));
            break;
        }
        budget -= row.len();
        rows.push(row.iter().map(|c| escape(c)).collect());
    }
    // 文件末尾的空行
    while rows.last().is_some_and(|r| r.iter().all(String::is_empty)) {
        rows.pop();
    }
    if wide {
        out.warnings
            .push(format!("columns beyond {max_cols} were dropped"));
    }
    out.parts = rows.len();
    out.markdown = if rows.is_empty() {
        String::new()
    } else {
        render(&[Block::Table(rows)])
    };
    Ok(out)
}

fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, ..] | [0xFE, 0xFF, ..] => crate::xml::decode(bytes.to_vec()),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(bytes)
                .0
                .into_owned(),
        },
    }
}

/// 在前 50 条记录里，哪个候选分隔符让每行字段数最一致（且多于 1 列）。都不像时用逗号。
fn sniff(text: &str) -> char {
    let mut best = (',', 0usize, 0usize);
    for d in [',', ';', '\t', '|'] {
        let counts: Vec<usize> = Records::new(text, d)
            .take(50)
            .map(|r| r.len())
            .filter(|&n| n > 0)
            .collect();
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

impl Iterator for Records<'_> {
    type Item = Vec<String>;

    fn next(&mut self) -> Option<Vec<String>> {
        if self.pos >= self.s.len() {
            return None;
        }
        let mut row = Vec::new();
        let mut field = String::new();
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
                    row.push(std::mem::take(&mut field));
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
        row.push(field);
        self.pos += consumed;
        Some(row)
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
}
