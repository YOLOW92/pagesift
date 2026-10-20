//! CMap 解析：ToUnicode（bfchar/bfrange）和编码 CMap（codespace、cidchar/cidrange）。

use std::collections::HashMap;

use crate::encoding::glyph_to_unicode;
use crate::lexer::{Lexer, Operand};

/// 展开 bfrange/cidrange 的总条目上限，超出的区间按区间保存。
const MAX_EXPANDED: usize = 200_000;

#[derive(Debug, Clone)]
struct Codespace {
    len: usize,
    lo: [u8; 4],
    hi: [u8; 4],
}

#[derive(Debug, Clone)]
struct UniRange {
    lo: u32,
    hi: u32,
    base: Vec<u16>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CMap {
    codespace: Vec<Codespace>,
    uni: HashMap<u32, String>,
    uni_ranges: Vec<UniRange>,
    cid: HashMap<u32, u32>,
    cid_ranges: Vec<(u32, u32, u32)>,
    /// `usecmap` 引用的预定义 CMap 名字。
    pub(crate) parent: Option<Vec<u8>>,
}

fn code_of(bytes: &[u8]) -> Option<(usize, u32)> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    let mut v = 0u32;
    for &b in bytes {
        v = v << 8 | u32::from(b);
    }
    Some((bytes.len(), v))
}

/// 目标串按 UTF-16BE 解码；奇数长度的按单字节处理（不规范但常见）。
fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    if bytes.len() % 2 == 1 {
        return bytes.iter().map(|&b| u16::from(b)).collect();
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect()
}

fn units_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

impl CMap {
    pub(crate) fn parse(data: &[u8]) -> CMap {
        let mut cm = CMap::default();
        let mut expanded = 0usize;
        let mut lx = Lexer::new(data);
        while let Some(op) = lx.next_op() {
            let args = &op.args;
            match op.name.as_slice() {
                b"endcodespacerange" => {
                    for pair in args.as_chunks::<2>().0 {
                        if let (Operand::Str(lo), Operand::Str(hi)) = (&pair[0], &pair[1])
                            && !lo.is_empty()
                            && lo.len() == hi.len()
                            && lo.len() <= 4
                        {
                            let mut c = Codespace {
                                len: lo.len(),
                                lo: [0; 4],
                                hi: [0; 4],
                            };
                            c.lo[..lo.len()].copy_from_slice(lo);
                            c.hi[..hi.len()].copy_from_slice(hi);
                            cm.codespace.push(c);
                        }
                    }
                }
                b"endbfchar" => {
                    for pair in args.as_chunks::<2>().0 {
                        let Operand::Str(src) = &pair[0] else {
                            continue;
                        };
                        let Some((_, code)) = code_of(src) else {
                            continue;
                        };
                        let dst = match &pair[1] {
                            Operand::Str(d) => units_to_string(&utf16_units(d)),
                            Operand::Name(n) => match glyph_to_unicode(n) {
                                Some(s) => s,
                                None => continue,
                            },
                            _ => continue,
                        };
                        cm.uni.insert(code, dst);
                    }
                }
                b"endbfrange" => {
                    for tri in args.as_chunks::<3>().0 {
                        let (Operand::Str(lo), Operand::Str(hi)) = (&tri[0], &tri[1]) else {
                            continue;
                        };
                        let (Some((_, lo)), Some((_, hi))) = (code_of(lo), code_of(hi)) else {
                            continue;
                        };
                        if hi < lo {
                            continue;
                        }
                        match &tri[2] {
                            Operand::Str(d) => {
                                let base = utf16_units(d);
                                if base.is_empty() {
                                    continue;
                                }
                                let n = usize::try_from(hi - lo)
                                    .unwrap_or(usize::MAX)
                                    .saturating_add(1);
                                if expanded.saturating_add(n) <= MAX_EXPANDED {
                                    expanded += n;
                                    for code in lo..=hi {
                                        cm.uni.insert(code, offset_string(&base, code - lo));
                                    }
                                } else {
                                    cm.uni_ranges.push(UniRange { lo, hi, base });
                                }
                            }
                            Operand::Array(items) => {
                                for (i, it) in items.iter().enumerate() {
                                    let Some(code) = lo.checked_add(i as u32) else {
                                        break;
                                    };
                                    if code > hi {
                                        break;
                                    }
                                    if let Operand::Str(d) = it {
                                        cm.uni.insert(code, units_to_string(&utf16_units(d)));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"endcidchar" => {
                    for pair in args.as_chunks::<2>().0 {
                        if let (Operand::Str(src), Some(cid)) = (&pair[0], pair[1].num())
                            && let Some((_, code)) = code_of(src)
                        {
                            cm.cid.insert(code, cid.max(0.0) as u32);
                        }
                    }
                }
                b"endcidrange" => {
                    for tri in args.as_chunks::<3>().0 {
                        if let (Operand::Str(lo), Operand::Str(hi), Some(cid)) =
                            (&tri[0], &tri[1], tri[2].num())
                            && let (Some((_, lo)), Some((_, hi))) = (code_of(lo), code_of(hi))
                            && lo <= hi
                        {
                            cm.cid_ranges.push((lo, hi, cid.max(0.0) as u32));
                        }
                    }
                }
                b"usecmap" => {
                    if let Some(Operand::Name(n)) = args.last() {
                        cm.parent = Some(n.clone());
                    }
                }
                _ => {}
            }
        }
        cm.uni_ranges.sort_by_key(|r| r.lo);
        cm.cid_ranges.sort_by_key(|r| r.0);
        cm
    }

    pub(crate) fn has_codespace(&self) -> bool {
        !self.codespace.is_empty()
    }

    /// 按 codespace 切出下一个字符码的字节数。没有匹配时退回最短长度。
    pub(crate) fn code_len(&self, bytes: &[u8]) -> usize {
        let mut shortest = usize::MAX;
        for len in 1..=4 {
            for cs in self.codespace.iter().filter(|c| c.len == len) {
                shortest = shortest.min(len);
                if bytes.len() >= len
                    && (0..len).all(|k| cs.lo[k] <= bytes[k] && bytes[k] <= cs.hi[k])
                {
                    return len;
                }
            }
        }
        if shortest == usize::MAX { 1 } else { shortest }
    }

    pub(crate) fn unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.uni.get(&code) {
            return Some(s.clone());
        }
        let idx = self.uni_ranges.partition_point(|r| r.lo <= code);
        let r = self.uni_ranges.get(idx.checked_sub(1)?)?;
        (code <= r.hi).then(|| offset_string(&r.base, code - r.lo))
    }

    pub(crate) fn cid(&self, code: u32) -> Option<u32> {
        if let Some(&c) = self.cid.get(&code) {
            return Some(c);
        }
        let idx = self.cid_ranges.partition_point(|r| r.0 <= code);
        let &(lo, hi, base) = self.cid_ranges.get(idx.checked_sub(1)?)?;
        (code <= hi).then(|| base.saturating_add(code - lo))
    }

    pub(crate) fn is_empty_mapping(&self) -> bool {
        self.uni.is_empty() && self.uni_ranges.is_empty()
    }
}

/// bfrange 的目标值：最后一个 UTF-16 单元加上偏移。
fn offset_string(base: &[u16], off: u32) -> String {
    let mut units = base.to_vec();
    if let Some(last) = units.last_mut() {
        *last = last.wrapping_add(off as u16);
    }
    units_to_string(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def /CMapType 2 def
1 begincodespacerange <0000> <FFFF> endcodespacerange
2 beginbfchar <0003> <0020> <0024> <4E2D> endbfchar
2 beginbfrange <0010> <0012> <0041> <0020> <0021> [<00660069> <D83DDE00>] endbfrange
endcmap CMapName currentdict /CMap defineresource pop end end";

    #[test]
    fn parses_tounicode() {
        let cm = CMap::parse(SAMPLE);
        assert_eq!(cm.unicode(0x24).as_deref(), Some("中"));
        assert_eq!(cm.unicode(0x03).as_deref(), Some(" "));
        assert_eq!(cm.unicode(0x11).as_deref(), Some("B"));
        assert_eq!(cm.unicode(0x20).as_deref(), Some("fi"));
        assert_eq!(cm.unicode(0x21).as_deref(), Some("😀"));
        assert_eq!(cm.unicode(0x99), None);
        assert_eq!(cm.code_len(&[0x00, 0x24]), 2);
    }

    #[test]
    fn mixed_codespace() {
        let cm = CMap::parse(
            b"2 begincodespacerange <00> <80> <8140> <FEFE> endcodespacerange
1 begincidrange <8140> <817E> 633 endcidrange",
        );
        assert_eq!(cm.code_len(&[0x41, 0x81]), 1);
        assert_eq!(cm.code_len(&[0x81, 0x41]), 2);
        assert_eq!(cm.cid(0x8142), Some(635));
    }
}
