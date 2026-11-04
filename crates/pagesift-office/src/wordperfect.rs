//! WordPerfect 5.x、6.x 取文字：文件头 `\xFFWPC` 的第 4 字节处是正文偏移。正文是字节码：
//! 可打印字节是文字，定长和变长函数码跳过，换行、制表符还原。4.2 及更早没有这个文件头，
//! 编码也简单（高位字节是函数码），按同样的思路单独处理。样式和排版不要。

use crate::md::{self, Block};
use crate::{Converted, Error, Options, budget, check_deadline};

const MAGIC: &[u8] = b"\xFFWPC";

/// 产品类型 1（WordPerfect）、文件类型 10（文档）之外的不是文档。
pub(crate) fn is_wpc(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let text = if is_wpc(bytes) {
        let start = u32::from_le_bytes(
            bytes
                .get(4..8)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| Error::Malformed("WordPerfect header is truncated".into()))?,
        ) as usize;
        let major = bytes.get(10).copied().unwrap_or(0);
        let body = bytes
            .get(start..)
            .ok_or_else(|| Error::Malformed("WordPerfect body is missing".into()))?;
        // 主版本 0 是 5.x，大于等于 2 是 6.x 及之后
        if major == 0 {
            v5(body, opts)?
        } else {
            v6(body, opts)?
        }
    } else {
        v4(bytes, opts)?
    };
    let blocks: Vec<Block> = text
        .split('\n')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| Block::Para(md::escape(p)))
        .collect();
    let parts = blocks.len();
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

/// 5.x：0x20–0x7E 文字；0x0A、0x0C 硬回车和换页；0x0D 软回车当空格；0x80–0xBF 单字节函数；
/// 0xC0–0xCF 定长函数（0xC0 是扩展字符）；0xD0–0xFE 变长：子码、2 字节长度、内容。
fn v5(b: &[u8], opts: &Options<'_>) -> Result<String, Error> {
    /// 0xC0 起每个定长函数的总字节数（WP 5.1 文件格式文档）。
    const FIXED: [usize; 16] = [4, 9, 11, 3, 5, 6, 7, 4, 5, 6, 8, 42, 3, 3, 6, 4];
    let mut out = String::new();
    let mut i = 0usize;
    while i < b.len() {
        if i.is_multiple_of(4096) {
            check_deadline(opts)?;
            if budget::exhausted() {
                break;
            }
        }
        let c = b[i];
        match c {
            0x20..=0x7E => {
                out.push(char::from(c));
                i += 1;
            }
            0x0A | 0x0C => {
                out.push('\n');
                i += 1;
            }
            0x0D | 0x8C | 0x99 => {
                out.push(' ');
                i += 1;
            }
            0xA9..=0xAB => {
                out.push('-');
                i += 1;
            }
            0xC0 => {
                // 扩展字符：C0 字符 集 C0。集 1 的多国字符按 Latin-1 近似，其他丢掉
                if let (Some(&ch), Some(&set)) = (b.get(i + 1), b.get(i + 2))
                    && (set == 0 || set == 1)
                    && ch >= 0x20
                {
                    out.push(char::from(ch));
                }
                i += 4;
            }
            0xC1..=0xCF => i += FIXED[usize::from(c - 0xC0)],
            0xD0..=0xFE => {
                let Some(len) = b.get(i + 2..i + 4) else {
                    break;
                };
                i += 4 + usize::from(u16::from_le_bytes([len[0], len[1]]));
            }
            _ => i += 1,
        }
        budget::spend(1);
    }
    Ok(out)
}

/// 6.x：0x21–0x7E 文字；0x80 空格；0xCC、0xCF 硬回车；0xF0 扩展字符（F0 字符 集 F0）；
/// 0xD0–0xEF 变长（子码、2 字节总长）；0xF1–0xFE 定长；其余单字节。
fn v6(b: &[u8], opts: &Options<'_>) -> Result<String, Error> {
    let mut out = String::new();
    let mut i = 0usize;
    while i < b.len() {
        if i.is_multiple_of(4096) {
            check_deadline(opts)?;
            if budget::exhausted() {
                break;
            }
        }
        let c = b[i];
        match c {
            0x20..=0x7E => {
                out.push(char::from(c));
                i += 1;
            }
            0x80 | 0x81 | 0xC2 | 0xC3 | 0xCA | 0xCB => {
                out.push(' ');
                i += 1;
            }
            0x83..=0x86 => {
                out.push('-');
                i += 1;
            }
            0x87 | 0x88 | 0xC7..=0xC9 | 0xCC | 0xCF | 0xD0 if c != 0xD0 => {
                out.push('\n');
                i += 1;
            }
            0xF0 => {
                if let (Some(&ch), Some(&set)) = (b.get(i + 1), b.get(i + 2))
                    && (set == 0 || set == 1)
                    && ch >= 0x20
                {
                    out.push(char::from(ch));
                }
                i += 4;
            }
            0xD0..=0xEF => {
                // 硬回车这类函数（0xD0 的子码 1、4 等）也是分段
                if c == 0xD0 && matches!(b.get(i + 1), Some(1 | 4 | 7)) {
                    out.push('\n');
                }
                let Some(len) = b.get(i + 2..i + 4) else {
                    break;
                };
                let n = usize::from(u16::from_le_bytes([len[0], len[1]]));
                i += n.max(4);
            }
            0xF1..=0xFE => {
                // 定长：第二个字节开始，到同一个函数码结束
                let end = b[i + 1..].iter().position(|&x| x == c);
                i += end.map_or(b.len() - i, |e| e + 2);
            }
            _ => i += 1,
        }
        budget::spend(1);
    }
    Ok(out)
}

/// 4.2 及更早：没有文件头。0x0A、0x0D 回车；0x80–0xBF 单字节函数；0xC0–0xFE 成对的函数码，
/// 从开始码跳到同一个结束码。
fn v4(b: &[u8], opts: &Options<'_>) -> Result<String, Error> {
    let mut out = String::new();
    let mut i = 0usize;
    while i < b.len() {
        if i.is_multiple_of(4096) {
            check_deadline(opts)?;
            if budget::exhausted() {
                break;
            }
        }
        let c = b[i];
        match c {
            0x20..=0x7E => {
                out.push(char::from(c));
                i += 1;
            }
            0x0A => {
                out.push('\n');
                i += 1;
            }
            0x0D | 0x8C => {
                out.push(' ');
                i += 1;
            }
            0xC0..=0xFE => {
                let end = b[i + 1..].iter().take(1 << 16).position(|&x| x == c);
                i += end.map_or(1, |e| e + 2);
            }
            _ => i += 1,
        }
        budget::spend(1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wpc(major: u8, body: &[u8]) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        b.extend_from_slice(&14u32.to_le_bytes());
        b.extend_from_slice(&[1, 10, major, 0, 0, 0]);
        b.extend_from_slice(body);
        b
    }

    fn run(b: &[u8]) -> String {
        convert(b, &Options::default()).unwrap().markdown
    }

    #[test]
    fn v5_text_and_functions() {
        let body = [
            b"Hello\x0D".as_slice(),
            b"world\x0A",
            &[0xC3, 1, 0xC3],             // 定长函数
            &[0xD0, 1, 2, 0, b'x', b'y'], // 变长函数，内容不进正文
            b"next",
            &[0xC0, 0xE9, 1, 0xC0],
        ]
        .concat();
        assert_eq!(run(&wpc(0, &body)), "Hello world\n\nnext\u{e9}\n");
    }

    #[test]
    fn v6_text_and_functions() {
        let body = [
            b"One\x80two\xCC".as_slice(),
            &[0xD4, 1, 6, 0, 0, 0xD4], // 变长，长度含自身
            &[0xF2, 9, 9, 0xF2],       // 定长
            b"three",
        ]
        .concat();
        assert_eq!(run(&wpc(2, &body)), "One two\n\nthree\n");
    }

    #[test]
    fn old_files_without_a_header() {
        let body = [b"Old\x0D".as_slice(), &[0xC5, 1, 2, 0xC5], b"text\x0Amore"].concat();
        assert_eq!(run(&body), "Old text\n\nmore\n");
    }

    #[test]
    fn truncated_and_hostile_files() {
        assert!(convert(b"\xFFWPC\x01", &Options::default()).is_err());
        assert_eq!(run(&wpc(0, &[0xD0, 1])), "");
        assert_eq!(run(&wpc(2, &[0xD4, 1])), "");
        assert_eq!(run(&wpc(2, &[0xF2])), "");
        assert_eq!(run(&wpc(0, &[0xC1])), "");
    }
}
