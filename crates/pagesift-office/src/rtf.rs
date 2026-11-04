//! RTF 取文字（RTF 1.9.1 规范）：控制字逐个扫描，`\'hh` 按 `\ansicpg` 解码，`\uN` 是 Unicode，
//! 段落和表格行转成 Markdown。不输出字体表、样式表、图片这些“目的地”。
//! 分组嵌套有上限，防恶意构造；输出受预算限制。

use crate::md::{self, Block};
use crate::{Converted, Error, Options, budget, check_deadline};

/// 分组嵌套上限：足够正常文档，又挡得住一串 `{` 的恶意文件。
const MAX_GROUPS: usize = 1_024;

/// 这些目的地的内容不是正文（规范第“目的地”一节，只列常见的；`\*` 开头的未知目的地也跳过）。
const SKIP: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "footnote",
    "themedata",
    "colorschememapping",
    "datastore",
    "latentstyles",
    "listtable",
    "listoverridetable",
    "revtbl",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "filetbl",
    "bkmkstart",
    "bkmkend",
    "fldinst",
    "mmathPr",
    "wgrffmtfilter",
];

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    /// `\uN` 之后要跳过的替代字符数（`\ucN`，缺省 1）。
    uc: u32,
}

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err(Error::Malformed("not an RTF document".into()));
    }
    let mut p = Parser {
        bytes,
        pos: 0,
        codepage: 1252,
        para: String::new(),
        pending: Vec::new(),
        blocks: Vec::new(),
        row: Vec::new(),
        table: Vec::new(),
        title: None,
        high: None,
    };
    p.run(opts)?;
    let markdown = if p.blocks.is_empty() {
        String::new()
    } else {
        md::render(&p.blocks)
    };
    let parts = p.blocks.len();
    Ok(Converted {
        markdown,
        title: p.title,
        parts,
        ..Default::default()
    })
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    codepage: u32,
    para: String,
    /// 连续的 `\'hh` 字节攒起来一起解码（多字节字符集的一个字符可能占两个）。
    pending: Vec<u8>,
    blocks: Vec<Block>,
    row: Vec<String>,
    table: Vec<Vec<String>>,
    title: Option<String>,
    /// `\uN` 的 UTF-16 高位代理，等下一个 `\uN` 拼成一个字符。
    high: Option<u16>,
}

impl Parser<'_> {
    fn run(&mut self, opts: &Options<'_>) -> Result<(), Error> {
        let mut stack: Vec<Group> = Vec::new();
        let mut cur = Group { skip: false, uc: 1 };
        let mut steps = 0usize;
        while let Some(&b) = self.bytes.get(self.pos) {
            steps += 1;
            if steps.is_multiple_of(4096) {
                check_deadline(opts)?;
                if budget::exhausted() {
                    break;
                }
            }
            match b {
                b'{' => {
                    self.pos += 1;
                    if stack.len() >= MAX_GROUPS {
                        return Err(Error::TooLarge("RTF group nesting"));
                    }
                    stack.push(cur);
                    // `{\*\xxx`：不认识的可忽略目的地，整组跳过
                    if self.bytes[self.pos..].starts_with(b"\\*") {
                        cur.skip = true;
                    }
                }
                b'}' => {
                    self.pos += 1;
                    self.flush_pending();
                    match stack.pop() {
                        Some(g) => cur = g,
                        None => break,
                    }
                }
                b'\\' => self.control(&mut cur),
                b'\r' | b'\n' => self.pos += 1,
                _ => {
                    let start = self.pos;
                    while let Some(&c) = self.bytes.get(self.pos) {
                        if matches!(c, b'{' | b'}' | b'\\' | b'\r' | b'\n') {
                            break;
                        }
                        self.pos += 1;
                    }
                    if !cur.skip {
                        self.flush_pending();
                        // 正文里的高位字节按当前代码页解码
                        let chunk = self.bytes[start..self.pos].to_vec();
                        self.pending.extend_from_slice(&chunk);
                        self.flush_pending();
                    }
                }
            }
        }
        self.end_para();
        self.end_table();
        Ok(())
    }

    fn control(&mut self, cur: &mut Group) {
        self.pos += 1; // 反斜杠
        let Some(&c) = self.bytes.get(self.pos) else {
            return;
        };
        if !c.is_ascii_alphabetic() {
            self.pos += 1;
            match c {
                b'\'' => {
                    let hi = self.bytes.get(self.pos).copied().and_then(hex);
                    let lo = self.bytes.get(self.pos + 1).copied().and_then(hex);
                    if let (Some(hi), Some(lo)) = (hi, lo) {
                        self.pos += 2;
                        if !cur.skip {
                            self.pending.push(hi << 4 | lo);
                        }
                    }
                }
                b'~' => self.text(cur, "\u{a0}"),
                b'-' => {}
                b'_' => self.text(cur, "\u{2011}"),
                b'\\' | b'{' | b'}' => {
                    let s = [c];
                    self.text(cur, std::str::from_utf8(&s).unwrap_or(""));
                }
                b'\r' | b'\n' if !cur.skip => {
                    self.end_para();
                }
                _ => {}
            }
            return;
        }
        let start = self.pos;
        while self
            .bytes
            .get(self.pos)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            self.pos += 1;
        }
        let word = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
        let num_start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        let param: Option<i64> = std::str::from_utf8(&self.bytes[num_start..self.pos])
            .ok()
            .and_then(|s| s.parse().ok());
        // 控制字后面紧跟的一个空格属于控制字
        if self.bytes.get(self.pos) == Some(&b' ') {
            self.pos += 1;
        }
        if SKIP.contains(&word) {
            if word == "info" {
                self.read_info_title();
            }
            cur.skip = true;
            return;
        }
        match word {
            "ansicpg" => {
                if let Some(n) = param.and_then(|n| u32::try_from(n).ok()) {
                    self.codepage = n;
                }
            }
            "uc" => cur.uc = param.and_then(|n| u32::try_from(n).ok()).unwrap_or(1),
            "u" => {
                if let Some(n) = param {
                    self.unicode(cur, n);
                    self.skip_fallback(cur.uc);
                }
            }
            _ if cur.skip => {}
            "par" | "sect" | "page" => self.end_para(),
            "line" => self.text(cur, "\n"),
            "tab" => self.text(cur, "\t"),
            "emdash" => self.text(cur, "\u{2014}"),
            "endash" => self.text(cur, "\u{2013}"),
            "bullet" => self.text(cur, "\u{2022}"),
            "lquote" => self.text(cur, "\u{2018}"),
            "rquote" => self.text(cur, "\u{2019}"),
            "ldblquote" => self.text(cur, "\u{201c}"),
            "rdblquote" => self.text(cur, "\u{201d}"),
            "cell" | "nestcell" => {
                self.flush_pending();
                let cell = md::escape(std::mem::take(&mut self.para).trim());
                self.row.push(cell);
            }
            "row" | "nestrow" => {
                self.flush_pending();
                self.para.clear();
                let row = std::mem::take(&mut self.row);
                if !row.is_empty() {
                    self.table.push(row);
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, cur: &Group, s: &str) {
        if !cur.skip {
            self.flush_pending();
            self.para.push_str(s);
            budget::spend(s.len());
        }
    }

    fn unicode(&mut self, cur: &Group, n: i64) {
        if cur.skip {
            return;
        }
        self.flush_pending();
        // 规范里 N 是有符号 16 位：大于 32767 的字符写成负数
        let unit = n.rem_euclid(65_536) as u16;
        match (self.high.take(), unit) {
            (_, 0xD800..=0xDBFF) => self.high = Some(unit),
            (Some(h), 0xDC00..=0xDFFF) => {
                let c = 0x10000 + ((u32::from(h) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                self.para.push(char::from_u32(c).unwrap_or('\u{fffd}'));
            }
            (_, u) => self
                .para
                .push(char::from_u32(u32::from(u)).unwrap_or('\u{fffd}')),
        }
    }

    /// 跳过 `\uN` 后面给旧阅读器看的替代字符（一个字节、一个 `\'hh` 或一个控制字都算一个）。
    fn skip_fallback(&mut self, count: u32) {
        for _ in 0..count {
            match self.bytes.get(self.pos) {
                Some(b'\\') => {
                    if self.bytes.get(self.pos + 1) == Some(&b'\'') {
                        self.pos = (self.pos + 4).min(self.bytes.len());
                    } else {
                        return;
                    }
                }
                Some(b'{' | b'}') | None => return,
                Some(_) => self.pos += 1,
            }
        }
    }

    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let text = decode_codepage(&bytes, self.codepage);
        budget::spend(text.len());
        self.para.push_str(&text);
    }

    fn end_para(&mut self) {
        self.flush_pending();
        let para = std::mem::take(&mut self.para);
        let t = para.trim();
        if t.is_empty() {
            return;
        }
        self.end_table();
        self.blocks.push(Block::Para(md::escape(t)));
    }

    fn end_table(&mut self) {
        let table = std::mem::take(&mut self.table);
        if !table.is_empty() {
            self.blocks.push(Block::Table(table));
        }
    }

    /// `{\info{\title ...}...}`：只取标题，其余整组由调用方跳过。
    fn read_info_title(&mut self) {
        const KEY: &[u8] = b"\\title";
        let end = find_group_end(self.bytes, self.pos);
        let info = &self.bytes[self.pos..end];
        let Some(i) = info.windows(KEY.len()).position(|w| w == KEY) else {
            return;
        };
        let rest = &info[i + KEY.len()..];
        let stop = rest.iter().position(|&b| b == b'}').unwrap_or(rest.len());
        let raw = &rest[..stop];
        // 标题里的转义不值得单独解析：纯文本才用，否则留空让调用方用别的来源
        if !raw.contains(&b'\\') {
            let t = decode_codepage(raw, self.codepage);
            let t = t.trim();
            if !t.is_empty() {
                self.title = Some(t.to_owned());
            }
        }
    }
}

/// `pos` 所在分组结束的位置（括号配对，跳过转义）。
fn find_group_end(bytes: &[u8], pos: usize) -> usize {
    let mut depth = 1usize;
    let mut i = pos;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

fn hex(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

/// 单字节和常见双字节代码页。UTF-8（65001）直接解；其余没有表的按 Windows-1252 兜底。
fn decode_codepage(bytes: &[u8], codepage: u32) -> String {
    if bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let label = match codepage {
        65001 => return String::from_utf8_lossy(bytes).into_owned(),
        932 => "shift_jis",
        936 => "gbk",
        949 => "euc-kr",
        950 => "big5",
        874 => "windows-874",
        1250 => "windows-1250",
        1251 => "windows-1251",
        1253 => "windows-1253",
        1254 => "windows-1254",
        1255 => "windows-1255",
        1256 => "windows-1256",
        1257 => "windows-1257",
        1258 => "windows-1258",
        _ => "windows-1252",
    };
    encoding_rs::Encoding::for_label(label.as_bytes())
        .unwrap_or(encoding_rs::WINDOWS_1252)
        .decode(bytes)
        .0
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &[u8]) -> Converted {
        convert(s, &Options::default()).unwrap()
    }

    #[test]
    fn paragraphs_escapes_and_destinations() {
        let d = run(
            br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}{\info{\title Doc}}\f0 Hello\par caf\'e9 \{x\}\line next\par{\*\unknown hidden}}",
        );
        assert_eq!(d.title.as_deref(), Some("Doc"));
        assert_eq!(d.markdown, "Hello\n\ncafé {x}\nnext\n");
    }

    #[test]
    fn unicode_with_fallback_and_codepages() {
        assert_eq!(
            run(br"{\rtf1\uc1 \u20013?\u25991?\par}").markdown,
            "\u{4e2d}\u{6587}\n"
        );
        // 负数写法和代理对
        assert_eq!(
            run(br"{\rtf1\u-10179?\u-8704?\par}").markdown,
            "\u{1f600}\n"
        );
        assert_eq!(
            run(br"{\rtf1\ansicpg936 \'d6\'d0\'ce\'c4\par}").markdown,
            "\u{4e2d}\u{6587}\n"
        );
    }

    #[test]
    fn tables_become_markdown_tables() {
        let d = run(br"{\rtf1 a\cell b\cell\row c\cell d\cell\row\pard after\par}");
        assert_eq!(d.markdown, "| a | b |\n| --- | --- |\n| c | d |\n\nafter\n");
    }

    #[test]
    fn hostile_input_stops_cleanly() {
        let deep = [b"{\\rtf1".as_slice(), &[b'{'; 5_000]].concat();
        assert_eq!(
            convert(&deep, &Options::default()),
            Err(Error::TooLarge("RTF group nesting"))
        );
        assert!(matches!(
            convert(b"plain", &Options::default()),
            Err(Error::Malformed(_))
        ));
        // 截断的文件照样返回已读到的内容
        assert_eq!(run(br"{\rtf1 partial \'").markdown, "partial\n");
    }
}
