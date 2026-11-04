//! Word 97–2003（.doc，MS-DOC）取文字：FIB 找到片段表（CLX），片段表把字符位置映射到
//! WordDocument 流里的文字（压缩片段是 cp1252，其余是 UTF-16）。字段只留结果，表格按单元格标记还原。
//! 标题和列表来自段落样式（见 `doc_styles`）；脚注、尾注、文本框排在正文后面。Word 6/95 另走单字节路径。

use super::doc_lists::Lists;
use super::doc_styles::{Kind, Styles};
use crate::md::{self, Block};
use crate::ole::Ole;
use crate::{Converted, Error, Options, budget, check_deadline};
use std::collections::HashMap;

fn u16_at(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?))
}

fn stream(ole: &Ole<'_>, name: &str) -> Result<Option<Vec<u8>>, Error> {
    match ole
        .children(None)
        .into_iter()
        .find(|e| e.is_stream && e.name.eq_ignore_ascii_case(name))
    {
        Some(e) => ole.read(e).map(Some),
        None => Ok(None),
    }
}

/// Word 97 起 nFib 是 193 以上；100–105 是 Word 6/95，FIB 结构不同。
const FIRST_97_FIB: u16 = 190;
const FIB_FC_STSHF: usize = 1;
const FIB_FC_PLCF_PAPX: usize = 13;
const FIB_FC_CLX: usize = 33;
const FIB_FC_PLF_LST: usize = 73;
const FIB_FC_PLF_LFO: usize = 74;

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let ole = Ole::open(bytes)?;
    let word = stream(&ole, "WordDocument")?
        .ok_or_else(|| Error::Malformed("no WordDocument stream".into()))?;
    let bad = || Error::Malformed("broken Word header".into());
    let flags = u16_at(&word, 0x0A).ok_or_else(bad)?;
    if flags & 0x0100 != 0 {
        return Err(Error::Encrypted);
    }
    let n_fib = u16_at(&word, 2).ok_or_else(bad)?;
    let blocks = if n_fib < FIRST_97_FIB {
        word6_blocks(&word, flags, opts)?
    } else {
        word97_blocks(&ole, &word, flags, opts)?
    };
    let mut out = Converted {
        parts: blocks.len(),
        markdown: if blocks.is_empty() {
            String::new()
        } else {
            md::render(&blocks)
        },
        ..Default::default()
    };
    super::props::apply(&ole, &mut out);
    Ok(out)
}

/// 一段连续文字在 WordDocument 流里的位置。
#[derive(Debug, Clone, Copy)]
struct Piece {
    /// 这段第一个字符在输出里的序号。
    first_char: usize,
    chars: usize,
    fc: u32,
    width: u32,
}

#[derive(Default)]
struct Text {
    chars: Vec<char>,
    pieces: Vec<Piece>,
}

impl Text {
    fn fc_of(&self, char_index: usize) -> Option<u32> {
        let i = self
            .pieces
            .partition_point(|p| p.first_char + p.chars <= char_index);
        let p = self.pieces.get(i).filter(|p| p.first_char <= char_index)?;
        Some(p.fc + (char_index - p.first_char) as u32 * p.width)
    }
}

fn word97_blocks(
    ole: &Ole<'_>,
    word: &[u8],
    flags: u16,
    opts: &Options<'_>,
) -> Result<Vec<Block>, Error> {
    let bad = || Error::Malformed("broken Word header".into());
    // FibBase 32 字节，之后是 csw、fibRgW、cslw、fibRgLw、cbRgFcLcb、fibRgFcLcb
    let csw = usize::from(u16_at(word, 32).ok_or_else(bad)?);
    let lw_len_at = 34 + csw * 2;
    let cslw = usize::from(u16_at(word, lw_len_at).ok_or_else(bad)?);
    let lw_at = lw_len_at + 2;
    let fc_at = lw_at + cslw * 4 + 2;
    let ccp = |i: usize| u32_at(word, lw_at + i * 4).unwrap_or(0) as usize;
    // 同一个字符序号空间里依次是：正文、脚注、页眉、批注、尾注、文本框、页眉文本框
    let (main, ftn, hdd, atn, edn, txbx, hdr_txbx) =
        (ccp(3), ccp(4), ccp(5), ccp(7), ccp(8), ccp(9), ccp(10));
    let total = [main, ftn, hdd, atn, edn, txbx, hdr_txbx]
        .iter()
        .fold(0usize, |a, b| a.saturating_add(*b));
    let table_name = if flags & 0x0200 != 0 {
        "1Table"
    } else {
        "0Table"
    };
    let table = stream(ole, table_name)?
        .ok_or_else(|| Error::Malformed(format!("no {table_name} stream")))?;
    let pair = |i: usize| -> Option<&[u8]> {
        let at = u32_at(word, fc_at + i * 8)? as usize;
        let len = u32_at(word, fc_at + i * 8 + 4)? as usize;
        table.get(at..at.checked_add(len)?)
    };
    let clx =
        pair(FIB_FC_CLX).ok_or_else(|| Error::Malformed("piece table is out of range".into()))?;
    let text = pieces(word, clx, total, opts)?;
    let styles = Styles::read(
        word,
        pair(FIB_FC_PLCF_PAPX).unwrap_or(&[]),
        pair(FIB_FC_STSHF).unwrap_or(&[]),
        {
            let lst_at = u32_at(word, fc_at + FIB_FC_PLF_LST * 8).unwrap_or(0) as usize;
            let lst_len = u32_at(word, fc_at + FIB_FC_PLF_LST * 8 + 4).unwrap_or(0) as usize;
            Lists::read(
                table.get(lst_at..).unwrap_or(&[]),
                lst_len,
                pair(FIB_FC_PLF_LFO).unwrap_or(&[]),
            )
        },
    );
    let at = |index: usize| text.fc_of(index).and_then(|fc| styles.kind_at(fc));
    let mut blocks = to_blocks(&text.chars, 0, main, &at);
    // 脚注、尾注、文本框排在正文后面，再接页眉页脚和批注；相同的页眉页脚（每节重复）只留一份
    let spans = [
        (main, ftn),
        (main + ftn + hdd + atn, edn),
        (main + ftn + hdd + atn + edn, txbx),
    ];
    for (start, len) in spans {
        blocks.extend(to_blocks(&text.chars, start, len, &|_| None));
    }
    let mut seen = std::collections::HashSet::new();
    for (start, len, tag) in [(main + ftn, hdd, ""), (main + ftn + hdd, atn, "[comment] ")] {
        for b in to_blocks(&text.chars, start, len, &|_| None) {
            if let Block::Para(t) = b
                && seen.insert((tag, t.clone()))
            {
                blocks.push(Block::Para(format!("{tag}{t}")));
            }
        }
    }
    Ok(blocks)
}

/// 走片段表，把文字接起来，到 `limit` 个字符为止。
fn pieces(word: &[u8], clx: &[u8], limit: usize, opts: &Options<'_>) -> Result<Text, Error> {
    let bad = || Error::Malformed("broken piece table".into());
    // Prc（类型 1）可以有多个，跳到 Pcdt（类型 2）
    let mut p = 0usize;
    while clx.get(p) == Some(&1) {
        p += 3 + usize::from(u16_at(clx, p + 1).ok_or_else(bad)?);
    }
    if clx.get(p) != Some(&2) {
        return Err(bad());
    }
    let size = u32_at(clx, p + 1).ok_or_else(bad)? as usize;
    let plc = clx
        .get(p + 5..(p + 5).saturating_add(size))
        .ok_or_else(bad)?;
    // n 个片段：(n+1) 个 4 字节字符位置 + n 个 8 字节描述
    let mut text = Text::default();
    if plc.len() < 4 {
        return Ok(text);
    }
    let n = (plc.len() - 4) / 12;
    let mut done = 0usize;
    for i in 0..n {
        check_deadline(opts)?;
        if done >= limit || budget::exhausted() {
            break;
        }
        let start = u32_at(plc, i * 4).ok_or_else(bad)? as usize;
        let end = u32_at(plc, (i + 1) * 4).ok_or_else(bad)? as usize;
        let chars = end.saturating_sub(start).min(limit - done);
        let raw_fc = u32_at(plc, (n + 1) * 4 + i * 8 + 2).ok_or_else(bad)?;
        let compressed = raw_fc & 0x4000_0000 != 0;
        let fc = raw_fc & 0x3FFF_FFFF;
        let first_char = text.chars.len();
        let (fc, width) = if compressed {
            let at = (fc / 2) as usize;
            let Some(slice) = word.get(at..at.saturating_add(chars)) else {
                continue;
            };
            text.chars
                .extend(encoding_rs::WINDOWS_1252.decode(slice).0.chars());
            (fc / 2, 1)
        } else {
            let at = fc as usize;
            let Some(slice) = word.get(at..at.saturating_add(chars.saturating_mul(2))) else {
                continue;
            };
            text.chars.extend(
                char::decode_utf16(
                    slice
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes([c[0], c[1]])),
                )
                .map(|c| c.unwrap_or('\u{fffd}')),
            );
            (fc, 2)
        };
        let added = text.chars.len() - first_char;
        budget::spend(added);
        text.pieces.push(Piece {
            first_char,
            chars: added,
            fc,
            width,
        });
        done += chars;
    }
    Ok(text)
}

/// Word 6/95：没有表流，单字节文字。快速保存的文件有片段表，普通文件正文在 fcMin 开始的连续区域。
fn word6_blocks(word: &[u8], flags: u16, opts: &Options<'_>) -> Result<Vec<Block>, Error> {
    let at = |i| u32_at(word, i).unwrap_or(0) as usize;
    let ccp_text = at(0x34);
    let codepage = word6_codepage(u16_at(word, 6).unwrap_or(0));
    let mut raw = Vec::new();
    let clx = word
        .get(at(0x1A2)..at(0x1A2).saturating_add(at(0x1A6)))
        .filter(|c| flags & 0x0004 != 0 && !c.is_empty());
    if let Some(clx) = clx {
        // 片段的 fc 直接是字节位置
        let mut p = 0usize;
        while clx.get(p) == Some(&1) {
            p += 3 + usize::from(u16_at(clx, p + 1).unwrap_or(0));
        }
        if clx.get(p) == Some(&2) {
            let size = u32_at(clx, p + 1).unwrap_or(0) as usize;
            if let Some(plc) = clx
                .get(p + 5..(p + 5).saturating_add(size))
                .filter(|d| d.len() >= 4)
            {
                let n = (plc.len() - 4) / 12;
                for i in 0..n {
                    check_deadline(opts)?;
                    let start = u32_at(plc, i * 4).unwrap_or(0) as usize;
                    let end = u32_at(plc, (i + 1) * 4).unwrap_or(0) as usize;
                    let fc = u32_at(plc, (n + 1) * 4 + i * 8 + 2).unwrap_or(0) as usize;
                    let take = end
                        .saturating_sub(start)
                        .min(ccp_text.saturating_sub(raw.len()));
                    if let Some(s) = word.get(fc..fc.saturating_add(take)) {
                        raw.extend_from_slice(s);
                    }
                }
            }
        }
    }
    if raw.is_empty() {
        let (min, max) = (at(0x18), at(0x1C));
        let end = if ccp_text > 0 {
            min.saturating_add(ccp_text).min(max)
        } else {
            max
        };
        raw = word.get(min..end).unwrap_or(&[]).to_vec();
    }
    budget::spend(raw.len());
    let chars: Vec<char> = super::props::decode(&raw, codepage).chars().collect();
    let len = chars.len();
    Ok(to_blocks(&chars, 0, len, &|_| None))
}

/// 按语言号猜 Word 6 的代码页。
fn word6_codepage(lid: u16) -> u16 {
    match lid {
        0x0404 | 0x0C04 | 0x1404 => 950,
        0x0804 | 0x1004 => 936,
        _ => match lid & 0x3FF {
            0x19 | 0x22 | 0x23 | 0x02 => 1251,
            0x05 | 0x15 | 0x0E | 0x1A | 0x18 | 0x1B | 0x24 => 1250,
            0x08 => 1253,
            0x1F => 1254,
            0x0D => 1255,
            0x01 => 1256,
            0x11 => 932,
            0x12 => 949,
            0x1E => 874,
            _ => 1252,
        },
    }
}

/// 特殊字符还原成段落和表格。域：`0x13 指令 0x14 结果 0x15`，只留结果。
/// `kind` 按字符序号查段落类型（参数是段落结束符的序号）。
/// 同一个列表里每一级的计数：本级加一，更深的级别重新开始。
fn next_number(counters: &mut HashMap<u32, [u32; 9]>, list: u32, level: u8, start: u32) -> u32 {
    let c = counters.entry(list).or_insert([0; 9]);
    let l = usize::from(level).min(8);
    c[l] = if c[l] == 0 {
        start.max(1)
    } else {
        c[l].saturating_add(1)
    };
    c[l + 1..].fill(0);
    c[l]
}

fn to_blocks(
    chars: &[char],
    start: usize,
    len: usize,
    kind: &dyn Fn(usize) -> Option<Kind>,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut cells: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    // 域可以嵌套：栈里记每层是否还在指令部分
    let mut fields: Vec<bool> = Vec::new();
    let mut prev_cell = false;
    let mut counters: HashMap<u32, [u32; 9]> = HashMap::new();
    let flush_table = |rows: &mut Vec<Vec<String>>, blocks: &mut Vec<Block>| {
        if rows.is_empty() {
            return;
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        for r in rows.iter_mut() {
            r.resize(width, String::new());
        }
        blocks.push(Block::Table(std::mem::take(rows)));
    };
    let end = start.saturating_add(len).min(chars.len());
    let span = chars.get(start..end).unwrap_or(&[]);
    for (i, &c) in span.iter().enumerate() {
        let cell_mark = c == '\u{7}';
        match c {
            '\u{13}' => fields.push(true),
            '\u{14}' => {
                if let Some(f) = fields.last_mut() {
                    *f = false;
                }
            }
            '\u{15}' => {
                fields.pop();
            }
            _ if fields.last() == Some(&true) => {}
            // 连续两个单元格标记：第二个是行尾
            '\u{7}' if prev_cell => rows.push(std::mem::take(&mut cells)),
            '\u{7}' => cells.push(md::escape(&md::squeeze(std::mem::take(&mut para).trim()))),
            '\r' | '\u{c}' => {
                let t = std::mem::take(&mut para);
                let t = t.trim();
                if !t.is_empty() {
                    flush_table(&mut rows, &mut blocks);
                    let text = md::escape(t);
                    blocks.push(match kind(start + i) {
                        Some(Kind::Heading(level)) => Block::Heading(level, text),
                        Some(Kind::Item {
                            level,
                            ordered,
                            start,
                            list,
                        }) => {
                            let number = if ordered {
                                next_number(&mut counters, list, level, start)
                            } else {
                                0
                            };
                            Block::Item {
                                level,
                                ordered,
                                number,
                                text,
                            }
                        }
                        None => Block::Para(text),
                    });
                }
            }
            '\u{b}' => para.push('\n'),
            '\t' => para.push('\t'),
            '\u{1e}' => para.push('-'),
            '\u{a0}' => para.push(' '),
            // 其余控制字符是图片、脚注这类占位
            c if (c as u32) < 0x20 => {}
            c => para.push(c),
        }
        prev_cell = cell_mark && fields.last() != Some(&true);
    }
    let t = para.trim().to_owned();
    if !t.is_empty() {
        flush_table(&mut rows, &mut blocks);
        blocks.push(Block::Para(md::escape(&t)));
    }
    flush_table(&mut rows, &mut blocks);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole::tests::{Node, build};

    /// 造一个最小的 Word 97 文件：一个压缩片段和一个 UTF-16 片段。
    fn sample(flags: u16) -> Vec<u8> {
        let text1 = b"Hello\r";
        let text2: Vec<u8> = "\u{4e2d}\u{6587}\r"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut word = vec![0u8; 1024];
        word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        word[2..4].copy_from_slice(&193u16.to_le_bytes());
        word[0x0A..0x0C].copy_from_slice(&flags.to_le_bytes());
        word[32..34].copy_from_slice(&14u16.to_le_bytes());
        let lw_len = 34 + 14 * 2;
        word[lw_len..lw_len + 2].copy_from_slice(&22u16.to_le_bytes());
        let lw = lw_len + 2;
        word[lw + 12..lw + 16].copy_from_slice(&9u32.to_le_bytes());
        let fc = lw + 22 * 4 + 2;
        word[fc - 2..fc].copy_from_slice(&93u16.to_le_bytes());
        let (t1_at, t2_at) = (2000usize, 2100usize);
        word.resize(3000, 0);
        word[t1_at..t1_at + text1.len()].copy_from_slice(text1);
        word[t2_at..t2_at + text2.len()].copy_from_slice(&text2);
        // CLX：Pcdt，2 个片段
        let mut plc = Vec::new();
        for cp in [0u32, 6, 9] {
            plc.extend_from_slice(&cp.to_le_bytes());
        }
        for (fc_raw,) in [(((t1_at * 2) as u32) | 0x4000_0000,), (t2_at as u32,)] {
            plc.extend_from_slice(&[0, 0]);
            plc.extend_from_slice(&fc_raw.to_le_bytes());
            plc.extend_from_slice(&[0, 0]);
        }
        let mut clx = vec![2u8];
        clx.extend_from_slice(&(plc.len() as u32).to_le_bytes());
        clx.extend_from_slice(&plc);
        word[fc + 33 * 8..fc + 33 * 8 + 4].copy_from_slice(&0u32.to_le_bytes());
        word[fc + 33 * 8 + 4..fc + 33 * 8 + 8].copy_from_slice(&(clx.len() as u32).to_le_bytes());
        build(&[
            Node {
                name: "WordDocument",
                data: &word,
                storage: false,
                parent: 0,
            },
            Node {
                name: "0Table",
                data: &clx,
                storage: false,
                parent: 0,
            },
        ])
    }

    #[test]
    fn reads_compressed_and_unicode_pieces() {
        let d = convert(&sample(0), &Options::default()).unwrap();
        assert_eq!(d.markdown, "Hello\n\n\u{4e2d}\u{6587}\n");
    }

    #[test]
    fn encrypted_files_are_reported() {
        assert_eq!(
            convert(&sample(0x0100), &Options::default()),
            Err(Error::Encrypted)
        );
    }

    #[test]
    fn fields_tables_and_breaks() {
        let chars: Vec<char> = "a\u{13}PAGEREF x\u{14}7\u{15}b\rc1\u{7}c2\u{7}\u{7}after\r"
            .chars()
            .collect();
        let b = to_blocks(&chars, 0, chars.len(), &|_| None);
        assert_eq!(
            b,
            vec![
                Block::Para("a7b".into()),
                Block::Table(vec![vec!["c1".into(), "c2".into()]]),
                Block::Para("after".into()),
            ]
        );
    }

    #[test]
    fn ordered_numbers_continue_and_restart() {
        let mut c = HashMap::new();
        let got: Vec<u32> = [(0, 1), (0, 1), (1, 1), (1, 1), (0, 1)]
            .iter()
            .map(|&(level, start)| next_number(&mut c, 7, level, start))
            .collect();
        assert_eq!(got, vec![1, 2, 1, 2, 3]);
    }
}
