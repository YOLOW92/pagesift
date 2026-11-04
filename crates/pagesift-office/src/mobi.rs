//! Kindle / Palm 电子书（.mobi、.azw、.azw3、.prc）：PDB 容器，第 0 条记录是头，后面是压缩的正文。
//! 压缩有三种：无、PalmDOC LZ77、HUFF/CDIC。正文是 HTML，交给 [`Options::html`]，和 EPUB 走同一条路。
//! 带 DRM 的报“已加密”。

use crate::{Converted, Error, Options, check_deadline, md};

/// 解压后的正文上限：书再长也不会超过这个量。
const MAX_TEXT: usize = 256 << 20;
const MAX_HUFF_DEPTH: usize = 32;

pub(crate) fn is_pdb_book(bytes: &[u8]) -> bool {
    matches!(bytes.get(60..68), Some(b"BOOKMOBI" | b"TEXtREAd"))
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_be_bytes(
        d.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn be32(d: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?) as usize)
}

struct Pdb<'a> {
    data: &'a [u8],
    offsets: Vec<usize>,
}

impl<'a> Pdb<'a> {
    fn open(data: &'a [u8]) -> Result<Pdb<'a>, Error> {
        let bad = || Error::Malformed("broken PDB header".into());
        let n = be16(data, 76).ok_or_else(bad)?;
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            offsets.push(be32(data, 78 + i * 8).ok_or_else(bad)?);
        }
        Ok(Pdb { data, offsets })
    }

    fn record(&self, i: usize) -> Option<&'a [u8]> {
        let start = *self.offsets.get(i)?;
        let end = self.offsets.get(i + 1).copied().unwrap_or(self.data.len());
        self.data.get(start..end.max(start).min(self.data.len()))
    }
}

/// PalmDOC LZ77。
fn palmdoc(input: &[u8], out: &mut Vec<u8>) {
    let start = out.len();
    let mut i = 0usize;
    while i < input.len() {
        let c = input[i];
        i += 1;
        match c {
            1..=8 => {
                let end = (i + usize::from(c)).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            0 | 9..=0x7F => out.push(c),
            0x80..=0xBF => {
                let Some(&next) = input.get(i) else { break };
                i += 1;
                let v = (usize::from(c) << 8 | usize::from(next)) & 0x3FFF;
                let dist = v >> 3;
                let len = (v & 7) + 3;
                if dist == 0 || dist > out.len() - start {
                    continue;
                }
                for _ in 0..len {
                    out.push(out[out.len() - dist]);
                }
            }
            _ => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
        }
    }
}

/// 每条记录尾部的附加数据（索引、多字节字符的续接）不属于正文，先去掉。
fn trailing_size(data: &[u8], flags: u16) -> usize {
    fn entry(data: &[u8], mut size: usize) -> usize {
        let (mut result, mut bits) = (0usize, 0u32);
        while size > 0 {
            let v = data[size - 1];
            result |= usize::from(v & 0x7F) << bits;
            bits += 7;
            size -= 1;
            if v & 0x80 != 0 || bits >= 28 {
                break;
            }
        }
        result
    }
    let mut num = 0usize;
    let mut test = flags >> 1;
    while test != 0 {
        if test & 1 != 0 {
            num += entry(data, data.len().saturating_sub(num));
        }
        test >>= 1;
    }
    if flags & 1 != 0 {
        let at = data.len().saturating_sub(num + 1);
        if let Some(b) = data.get(at) {
            num += usize::from(b & 3) + 1;
        }
    }
    num.min(data.len())
}

/// HUFF/CDIC（calibre 和 KindleUnpack 记录的格式）：前缀码查短语表，短语本身可能再次压缩。
struct Huff<'a> {
    dict1: Vec<(u32, bool, u32)>,
    mincode: [u32; 33],
    maxcode: [u32; 33],
    phrases: Vec<(&'a [u8], bool)>,
    /// 已经展开的短语。
    done: Vec<Option<Vec<u8>>>,
}

impl<'a> Huff<'a> {
    fn new(pdb: &Pdb<'a>, first: usize, count: usize) -> Option<Huff<'a>> {
        let huff = pdb.record(first)?;
        if huff.get(..4)? != b"HUFF" {
            return None;
        }
        let off1 = be32(huff, 8)?;
        let off2 = be32(huff, 12)?;
        let mut dict1 = Vec::with_capacity(256);
        for i in 0..256 {
            let v = be32(huff, off1 + i * 4)? as u32;
            let len = v & 0x1F;
            if len == 0 {
                return None;
            }
            let max = (((u64::from(v >> 8) + 1) << (32 - len)) - 1) as u32;
            dict1.push((len, v & 0x80 != 0, max));
        }
        let (mut mincode, mut maxcode) = ([0u32; 33], [0u32; 33]);
        for i in 1..=32usize {
            let lo = be32(huff, off2 + (i - 1) * 8)? as u64;
            let hi = be32(huff, off2 + (i - 1) * 8 + 4)? as u64;
            mincode[i] = (lo << (32 - i)) as u32;
            maxcode[i] = (((hi + 1) << (32 - i)).saturating_sub(1)) as u32;
        }
        let mut phrases = Vec::new();
        for r in first + 1..first + count {
            let cdic = pdb.record(r)?;
            if cdic.get(..4)? != b"CDIC" {
                return None;
            }
            let total = be32(cdic, 8)?;
            let bits = be32(cdic, 12)?;
            if bits > 24 {
                return None;
            }
            let n = (1usize << bits).min(total.saturating_sub(phrases.len()));
            for i in 0..n {
                let off = 16 + be16(cdic, 16 + i * 2)?;
                let blen = be16(cdic, off)?;
                let slice = cdic.get(off + 2..off + 2 + (blen & 0x7FFF))?;
                phrases.push((slice, blen & 0x8000 != 0));
            }
        }
        let done = vec![None; phrases.len()];
        Some(Huff {
            dict1,
            mincode,
            maxcode,
            phrases,
            done,
        })
    }

    fn unpack(&mut self, data: &[u8], depth: usize, out: &mut Vec<u8>) -> Option<()> {
        if depth > MAX_HUFF_DEPTH {
            return None;
        }
        let mut bits_left = (data.len() as i64) * 8;
        let read = |pos: usize| -> u64 {
            let mut v = 0u64;
            for k in 0..8 {
                v = v << 8 | u64::from(data.get(pos + k).copied().unwrap_or(0));
            }
            v
        };
        let mut pos = 0usize;
        let mut x = read(pos);
        let mut n = 32i32;
        loop {
            if n <= 0 {
                pos += 4;
                x = read(pos);
                n += 32;
            }
            let code = ((x >> n) & 0xFFFF_FFFF) as u32;
            let (mut len, term, mut max) = self.dict1[(code >> 24) as usize];
            if !term {
                while (len as usize) < 33 && code < self.mincode[len as usize] {
                    len += 1;
                }
                if len > 32 {
                    return None;
                }
                max = self.maxcode[len as usize];
            }
            n -= len as i32;
            bits_left -= i64::from(len);
            if bits_left < 0 {
                return Some(());
            }
            let r = (max.wrapping_sub(code) >> (32 - len)) as usize;
            let (slice, plain) = *self.phrases.get(r)?;
            if plain {
                out.extend_from_slice(slice);
            } else {
                if self.done[r].is_none() {
                    let mut buf = Vec::new();
                    self.unpack(slice, depth + 1, &mut buf)?;
                    self.done[r] = Some(buf);
                }
                out.extend_from_slice(self.done[r].as_deref()?);
            }
            if out.len() > MAX_TEXT {
                return None;
            }
        }
    }
}

pub(crate) fn convert(bytes: &[u8], opts: &Options<'_>) -> Result<Converted, Error> {
    let bad = || Error::Malformed("broken MOBI header".into());
    let pdb = Pdb::open(bytes)?;
    let head = pdb.record(0).ok_or_else(bad)?;
    let compression = be16(head, 0).ok_or_else(bad)?;
    let records = be16(head, 8).ok_or_else(bad)?;
    let is_mobi = head.get(16..20) == Some(b"MOBI");
    if is_mobi && be16(head, 12).ok_or_else(bad)? != 0 {
        return Err(Error::Encrypted);
    }
    let (flags, utf8, title, huff_first, huff_count) = if is_mobi {
        let len = be32(head, 0x14).ok_or_else(bad)?;
        let flags = if len >= 0xE4 {
            be16(head, 0xF2).unwrap_or(0) as u16
        } else {
            0
        };
        let name_off = be32(head, 0x54).unwrap_or(0);
        let name_len = be32(head, 0x58).unwrap_or(0);
        let utf8 = be32(head, 0x1C).unwrap_or(1252) == 65_001;
        let title = head
            .get(name_off..name_off.saturating_add(name_len))
            .map(|n| decode(n, utf8).trim().to_owned())
            .filter(|t| !t.is_empty());
        (
            flags,
            utf8,
            title,
            be32(head, 0x70).unwrap_or(0),
            be32(head, 0x74).unwrap_or(0),
        )
    } else {
        (0, false, None, 0, 0)
    };
    let mut huff = if compression == 17_480 {
        Some(
            Huff::new(&pdb, huff_first, huff_count)
                .ok_or_else(|| Error::Malformed("broken HUFF tables".into()))?,
        )
    } else {
        None
    };
    if !matches!(compression, 1 | 2 | 17_480) {
        return Err(Error::Unsupported(format!(
            "MOBI compression {compression}"
        )));
    }
    let mut text = Vec::new();
    for i in 1..=records {
        check_deadline(opts)?;
        let Some(rec) = pdb.record(i) else { break };
        let rec = &rec[..rec.len() - trailing_size(rec, flags)];
        match (compression, huff.as_mut()) {
            (2, _) => palmdoc(rec, &mut text),
            (_, Some(h)) => h
                .unpack(rec, 0, &mut text)
                .ok_or_else(|| Error::Malformed("broken HUFF data".into()))?,
            _ => text.extend_from_slice(rec),
        }
        if text.len() > MAX_TEXT {
            return Err(Error::TooLarge("decompressed size"));
        }
        // 输出预算按转换后算；这里只防解压炸弹
    }
    let body = decode(&text, utf8);
    // 老式 Mobipocket 的 DRM 没有 MOBI 头，解出来是乱码
    if !is_mobi && looks_scrambled(&text) {
        return Err(Error::Encrypted);
    }
    let head_lower = body
        .get(..body.len().min(4096))
        .unwrap_or(&body)
        .to_ascii_lowercase();
    let markdown = if is_mobi || (head_lower.contains("<html") && opts.html.is_some()) {
        let html = opts
            .html
            .ok_or_else(|| Error::Unsupported("no HTML converter for MOBI".into()))?;
        html(&body, "book.html")
    } else {
        // PalmDOC 文本书
        body.split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(md::escape)
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n"
    };
    Ok(Converted {
        markdown,
        title,
        parts: records,
        ..Default::default()
    })
}

/// 控制字符占比超过 5%：不是文字。
fn looks_scrambled(text: &[u8]) -> bool {
    let sample = &text[..text.len().min(64 << 10)];
    let bad = sample
        .iter()
        .filter(|&&b| b < 9 || (14..32).contains(&b))
        .count();
    !sample.is_empty() && bad * 20 > sample.len()
}

fn decode(bytes: &[u8], utf8: bool) -> String {
    if utf8 {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdb(kind: &[u8; 8], records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0u8; 78];
        out[60..68].copy_from_slice(kind);
        out[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        let mut at = 78 + records.len() * 8;
        for r in records {
            out.extend_from_slice(&(at as u32).to_be_bytes());
            out.extend_from_slice(&[0; 4]);
            at += r.len();
        }
        for r in records {
            out.extend_from_slice(r);
        }
        out
    }

    fn header(compression: u16, count: u16, encryption: u16, title: &str) -> Vec<u8> {
        let mut h = vec![0u8; 16];
        h[0..2].copy_from_slice(&compression.to_be_bytes());
        h[8..10].copy_from_slice(&count.to_be_bytes());
        h[12..14].copy_from_slice(&encryption.to_be_bytes());
        h.extend_from_slice(b"MOBI");
        h.resize(0x100, 0);
        h[0x14..0x18].copy_from_slice(&0xE8u32.to_be_bytes());
        h[0x1C..0x20].copy_from_slice(&65_001u32.to_be_bytes());
        h[0x54..0x58].copy_from_slice(&(0x100u32).to_be_bytes());
        h[0x58..0x5C].copy_from_slice(&(title.len() as u32).to_be_bytes());
        h.extend_from_slice(title.as_bytes());
        h
    }

    fn html(body: &str, _path: &str) -> String {
        body.replace("<p>", "").replace("</p>", "\n")
    }

    fn opts() -> Options<'static> {
        Options {
            html: Some(&html),
            ..Default::default()
        }
    }

    #[test]
    fn palmdoc_decoding() {
        let mut out = Vec::new();
        // 字面、带长度的字面、空格 + 字符、回指
        palmdoc(&[b'a', 2, 0xFF, 0xFE, 0xE1], &mut out);
        assert_eq!(out, vec![b'a', 0xFF, 0xFE, b' ', 0x61]);
        let mut out = Vec::new();
        // 回指距离 3、长度 3 + 2
        let v: u16 = 0x8000 | (3 << 3) | 2;
        palmdoc(&[b'a', b'b', b'c', (v >> 8) as u8, v as u8], &mut out);
        assert_eq!(out, b"abcabcab");
        // 回指超过已有内容不崩溃
        let mut out = Vec::new();
        palmdoc(&[0x80 | 0x3F, 0xFF, 0x80], &mut out);
        assert!(out.len() <= 2);
    }

    #[test]
    fn book_with_trailing_data_and_title() {
        // 附加标志 2：记录尾部一个变长长度，值含自身
        let mut body = b"<p>Hello \xE4\xB8\xAD</p>".to_vec();
        body.extend_from_slice(&[9, 9, 0x83]);
        let mut h = header(1, 1, 0, "My Book");
        h[0xF2..0xF4].copy_from_slice(&2u16.to_be_bytes());
        let bytes = pdb(b"BOOKMOBI", &[h, body]);
        assert!(is_pdb_book(&bytes));
        let d = convert(&bytes, &opts()).unwrap();
        assert_eq!(d.title.as_deref(), Some("My Book"));
        assert_eq!(d.markdown, "Hello \u{4e2d}\n");
    }

    #[test]
    fn drm_and_missing_converter() {
        let bytes = pdb(b"BOOKMOBI", &[header(1, 0, 2, "x")]);
        assert_eq!(convert(&bytes, &opts()), Err(Error::Encrypted));
        let bytes = pdb(b"BOOKMOBI", &[header(1, 0, 0, "x")]);
        assert!(matches!(
            convert(&bytes, &Options::default()),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn plain_palm_text() {
        let mut h = vec![0u8; 16];
        h[0..2].copy_from_slice(&1u16.to_be_bytes());
        h[8..10].copy_from_slice(&1u16.to_be_bytes());
        let bytes = pdb(b"TEXtREAd", &[h, b"one\n\ntwo".to_vec()]);
        assert_eq!(convert(&bytes, &opts()).unwrap().markdown, "one\n\ntwo\n");
        // 带 HTML 的文本书走 HTML；乱码报已加密
        let mut h = vec![0u8; 16];
        h[0..2].copy_from_slice(&1u16.to_be_bytes());
        h[8..10].copy_from_slice(&1u16.to_be_bytes());
        let bytes = pdb(b"TEXtREAd", &[h.clone(), b"<HTML><p>x</p></HTML>".to_vec()]);
        assert!(convert(&bytes, &opts()).unwrap().markdown.contains('x'));
        let bytes = pdb(b"TEXtREAd", &[h, vec![1u8; 64]]);
        assert_eq!(convert(&bytes, &opts()), Err(Error::Encrypted));
    }

    #[test]
    fn huff_cdic_roundtrip() {
        // 一个 8 位定长码：编码 = 255 - 短语号，第二个短语是嵌套压缩
        let mut huff = vec![0u8; 24];
        huff[..4].copy_from_slice(b"HUFF");
        huff[8..12].copy_from_slice(&24u32.to_be_bytes());
        huff[12..16].copy_from_slice(&((24 + 256 * 4) as u32).to_be_bytes());
        for code in 0..256u32 {
            let v = (255u32 << 8) | 0x80 | 8;
            let _ = code;
            huff.extend_from_slice(&v.to_be_bytes());
        }
        huff.extend_from_slice(&[0u8; 64 * 4]);
        let phrases: [(&[u8], bool); 2] = [(b"ab", true), (&[0xFF], false)];
        let mut table = Vec::new();
        let mut data = Vec::new();
        let base = (phrases.len() * 2) as u16;
        for (p, plain) in phrases {
            table.extend_from_slice(&(base + data.len() as u16).to_be_bytes());
            let blen = p.len() as u16 | if plain { 0x8000 } else { 0 };
            data.extend_from_slice(&blen.to_be_bytes());
            data.extend_from_slice(p);
        }
        let mut cdic = vec![0u8; 16];
        cdic[..4].copy_from_slice(b"CDIC");
        cdic[4..8].copy_from_slice(&16u32.to_be_bytes());
        cdic[8..12].copy_from_slice(&2u32.to_be_bytes());
        cdic[12..16].copy_from_slice(&8u32.to_be_bytes());
        cdic.extend(table);
        cdic.extend(data);
        // 正文：短语 1（嵌套，展开成短语 0）+ 短语 0
        let body = vec![0xFE, 0xFF];
        let mut h = header(17_480, 1, 0, "H");
        h[0x70..0x74].copy_from_slice(&2u32.to_be_bytes());
        h[0x74..0x78].copy_from_slice(&2u32.to_be_bytes());
        let bytes = pdb(b"BOOKMOBI", &[h, body, huff, cdic]);
        assert_eq!(convert(&bytes, &opts()).unwrap().markdown, "abab");
    }
}
