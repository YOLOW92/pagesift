//! 韩文文字处理器 HWP 5.0：OLE 复合文件，`FileHeader` 标明压缩和加密，
//! `BodyText/Section*` 是（通常 deflate 压缩的）记录流，正文在 PARA_TEXT 记录里，UTF-16。
//! 控制字符：内联和扩展控制占 8 个 UTF-16 单元，字符控制占 1 个（HWP 5.0 格式文档 §4.3.10）。

use crate::md::{self, Block};
use crate::ole::Ole;
use crate::{Converted, Error, Options, budget, check_deadline};

/// HWPTAG_BEGIN (0x10) + 51。
const TAG_PARA_TEXT: u32 = 0x43;
/// HWPTAG_BEGIN + 56。
const TAG_LIST_HEADER: u32 = 0x48;
/// HWPTAG_BEGIN + 61。
const TAG_TABLE: u32 = 0x4D;
const MAX_TABLE_SIDE: usize = 4_096;

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let ole = Ole::open(bytes)?;
    let root = ole.children(None);
    let header = root
        .iter()
        .find(|e| e.is_stream && e.name == "FileHeader")
        .ok_or_else(|| Error::Malformed("no FileHeader stream".into()))?;
    let header = ole.read(header)?;
    if !header.starts_with(b"HWP Document File") {
        return Err(Error::Malformed("not an HWP document".into()));
    }
    let flags = header.get(36).copied().unwrap_or(0);
    if flags & 0x02 != 0 {
        return Err(Error::Encrypted);
    }
    let compressed = flags & 0x01 != 0;
    let body = root
        .iter()
        .find(|e| !e.is_stream && e.name == "BodyText")
        .ok_or_else(|| Error::Malformed("no BodyText storage".into()))?;
    let mut sections = ole.children(Some(body));
    sections.retain(|e| e.is_stream && e.name.starts_with("Section"));
    sections.sort_by_key(|e| e.name["Section".len()..].parse::<u32>().unwrap_or(u32::MAX));
    let mut blocks = Vec::new();
    for s in &sections {
        check_deadline(opts)?;
        let raw = ole.read(s)?;
        let data = if compressed {
            inflate(&raw, opts.limits.max_entry_bytes)?
        } else {
            raw
        };
        read_records(&data, &mut blocks, opts)?;
        if budget::exhausted() {
            break;
        }
    }
    let parts = sections.len();
    Ok(Converted {
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        parts,
        ..Default::default()
    })
}

fn inflate(raw: &[u8], limit: u64) -> Result<Vec<u8>, Error> {
    use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_with_limit};
    let max = usize::try_from(limit).unwrap_or(usize::MAX);
    decompress_to_vec_with_limit(raw, max).map_err(|e| {
        if matches!(e.status, TINFLStatus::HasMoreOutput) {
            Error::TooLarge("decompressed size")
        } else {
            Error::Malformed("broken HWP section".into())
        }
    })
}

/// 表格在记录流里的样子：TABLE 记录之后，每个单元格一个 LIST_HEADER（带行列号），单元格的段落跟在它后面，
/// 段落头和 LIST_HEADER、TABLE 同层，段落文字更深一层；层级比 TABLE 浅就是表格结束。
struct Table {
    level: u32,
    rows: Vec<Vec<String>>,
    /// 当前单元格（行、列）。
    at: Option<(usize, usize)>,
}

impl Table {
    fn into_block(self) -> Option<Block> {
        let width = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return None;
        }
        let mut rows = self.rows;
        rows.retain(|r| r.iter().any(|c| !c.is_empty()));
        if rows.is_empty() {
            return None;
        }
        for r in &mut rows {
            r.resize(width, String::new());
        }
        Some(Block::Table(rows))
    }
}

/// 记录头 4 字节：低 10 位是标签，中间 10 位是层级，高 12 位是长度；长度全 1 时真实长度在后面 4 字节。
fn read_records(data: &[u8], blocks: &mut Vec<Block>, opts: &Options<'_>) -> Result<(), Error> {
    let mut p = 0usize;
    let mut n = 0usize;
    let mut table: Option<Table> = None;
    while let Some(h) = data.get(p..p + 4) {
        n += 1;
        if n.is_multiple_of(1024) {
            check_deadline(opts)?;
        }
        let head = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        let tag = head & 0x3FF;
        let level = (head >> 10) & 0x3FF;
        let mut size = (head >> 20) as usize;
        p += 4;
        if size == 0xFFF {
            let Some(s) = data.get(p..p + 4) else { break };
            size = u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize;
            p += 4;
        }
        let end = p.saturating_add(size).min(data.len());
        let body = &data[p..end];
        if let Some(t) = &table
            && level < t.level
            && let Some(b) = table.take().and_then(Table::into_block)
        {
            blocks.push(b);
        }
        match tag {
            TAG_TABLE if table.is_none() => {
                table = Some(Table {
                    level,
                    rows: Vec::new(),
                    at: None,
                });
            }
            TAG_LIST_HEADER => {
                // 嵌套表格的单元格层级更深：文字归到外层当前单元格
                if let Some(t) = table.as_mut().filter(|t| level == t.level) {
                    // 单元格属性在 8 字节列表头之后：列号、行号，都是 u16
                    let u16_at = |i: usize| {
                        body.get(i..i + 2)
                            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
                    };
                    if let (Some(col), Some(row)) = (u16_at(8), u16_at(10))
                        && col < MAX_TABLE_SIDE
                        && row < MAX_TABLE_SIDE
                    {
                        if t.rows.len() <= row {
                            t.rows.resize(row + 1, Vec::new());
                        }
                        if t.rows[row].len() <= col {
                            t.rows[row].resize(col + 1, String::new());
                        }
                        t.at = Some((row, col));
                    }
                }
            }
            TAG_PARA_TEXT => {
                let text = para_text(body);
                let text = text.trim();
                if !text.is_empty() {
                    budget::spend(text.len());
                    match table.as_mut().and_then(|t| Some((t.at?, t))) {
                        Some(((r, c), t)) => {
                            let cell = &mut t.rows[r][c];
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(&md::escape(&md::squeeze(text).replace('\n', " ")));
                        }
                        None => blocks.push(Block::Para(md::escape(text))),
                    }
                }
            }
            _ => {}
        }
        p = end;
    }
    if let Some(b) = table.take().and_then(Table::into_block) {
        blocks.push(b);
    }
    Ok(())
}

fn para_text(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut kept = Vec::with_capacity(units.len());
    let mut i = 0usize;
    while i < units.len() {
        let u = units[i];
        match u {
            // 字符控制：1 个单元
            0 | 24..=31 => i += 1,
            10 => {
                kept.push(u16::from(b'\n'));
                i += 1;
            }
            13 => i += 1,
            9 => {
                // 制表符也是内联控制，占 8 个单元
                kept.push(u16::from(b'\t'));
                i += 8;
            }
            // 内联和扩展控制：8 个单元
            1..=8 | 11 | 12 | 14..=23 => i += 8,
            _ => {
                kept.push(u);
                i += 1;
            }
        }
    }
    String::from_utf16_lossy(&kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole::tests::{Node, build};

    fn record(tag: u32, body: &[u8]) -> Vec<u8> {
        leveled(tag, 0, body)
    }

    fn leveled(tag: u32, level: u32, body: &[u8]) -> Vec<u8> {
        let head = tag | level << 10 | (body.len() as u32) << 20;
        [head.to_le_bytes().as_slice(), body].concat()
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn file(flags: u8, section: &[u8]) -> Vec<u8> {
        let mut header = b"HWP Document File".to_vec();
        header.resize(256, 0);
        header[36] = flags;
        build(&[
            Node {
                name: "FileHeader",
                data: &header,
                storage: false,
                parent: 0,
            },
            Node {
                name: "BodyText",
                data: b"",
                storage: true,
                parent: 0,
            },
            Node {
                name: "Section0",
                data: section,
                storage: false,
                parent: 2,
            },
        ])
    }

    #[test]
    fn reads_compressed_sections() {
        let mut para = utf16("\u{d55c}\u{ae00}");
        para.extend_from_slice(&utf16("\u{d}"));
        let plain = [record(0x42, b"ignored"), record(TAG_PARA_TEXT, &para)].concat();
        let z = miniz_oxide::deflate::compress_to_vec(&plain, 1);
        let d = convert(&file(1, &z), &Options::default()).unwrap();
        assert_eq!(d.markdown, "\u{d55c}\u{ae00}\n");
        assert_eq!(
            convert(&file(0, &plain), &Options::default())
                .unwrap()
                .markdown,
            "\u{d55c}\u{ae00}\n"
        );
    }

    #[test]
    fn tables_keep_rows_and_columns() {
        let cell = |row: u16, col: u16| {
            let mut b = vec![0u8; 8];
            b.extend_from_slice(&col.to_le_bytes());
            b.extend_from_slice(&row.to_le_bytes());
            b
        };
        let text = |t: &str| leveled(TAG_PARA_TEXT, 3, &utf16(t));
        let plain = [
            leveled(TAG_TABLE, 2, &[0; 8]),
            leveled(TAG_LIST_HEADER, 2, &cell(0, 0)),
            text("a"),
            leveled(TAG_LIST_HEADER, 2, &cell(0, 1)),
            text("b"),
            leveled(TAG_LIST_HEADER, 2, &cell(1, 1)),
            text("d"),
            leveled(TAG_PARA_TEXT, 0, &utf16("after")),
        ]
        .concat();
        let d = convert(&file(0, &plain), &Options::default()).unwrap();
        assert!(d.markdown.contains("| a | b |"), "{}", d.markdown);
        assert!(d.markdown.contains("|  | d |"), "{}", d.markdown);
        assert!(d.markdown.trim_end().ends_with("after"), "{}", d.markdown);
    }

    #[test]
    fn control_characters_take_their_width() {
        let mut units: Vec<u16> = vec![u16::from(b'a')];
        // 扩展控制占 8 个单元，内容不是文字
        units.extend([3, 99, 99, 99, 99, 99, 99, 3]);
        units.push(u16::from(b'b'));
        units.extend([9, 0, 0, 0, 0, 0, 0, 9]);
        units.push(u16::from(b'c'));
        units.push(10);
        units.push(u16::from(b'd'));
        let raw: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(para_text(&raw), "ab\tc\nd");
    }

    #[test]
    fn encrypted_and_wrong_files() {
        assert_eq!(
            convert(&file(2, b""), &Options::default()),
            Err(Error::Encrypted)
        );
        let other = build(&[Node {
            name: "FileHeader",
            data: b"nope",
            storage: false,
            parent: 0,
        }]);
        assert!(matches!(
            convert(&other, &Options::default()),
            Err(Error::Malformed(_))
        ));
    }
}
