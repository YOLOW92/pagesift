//! CFF (Compact Font Format / FontFile3 / Type1C) 内建编码解析。

use super::strings::STANDARD_STRINGS;

/// 预定义 ISOAdobe 字符集的字形数（.notdef + SID 1–228）。
const ISO_ADOBE_GLYPHS: usize = 229;

pub(crate) struct Reader<'a> {
    pub(crate) b: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn u8(&self, at: usize) -> Option<usize> {
        self.b.get(at).map(|&v| v as usize)
    }

    pub(crate) fn u16(&self, at: usize) -> Option<usize> {
        Some(self.u8(at)? << 8 | self.u8(at + 1)?)
    }

    pub(crate) fn offset(&self, at: usize, size: usize) -> Option<usize> {
        (0..size).try_fold(0usize, |v, i| Some(v << 8 | self.u8(at + i)?))
    }

    pub(crate) fn index(&self, at: usize) -> Option<(Vec<&'a [u8]>, usize)> {
        let count = self.u16(at)?;
        if count == 0 {
            return Some((Vec::new(), at + 2));
        }
        let size = self.u8(at + 2)?;
        if !(1..=4).contains(&size) {
            return None;
        }
        let offs = at + 3;
        let data = offs + (count + 1) * size - 1;
        let mut items = Vec::with_capacity(count);
        for i in 0..count {
            let a = data + self.offset(offs + i * size, size)?;
            let b = data + self.offset(offs + (i + 1) * size, size)?;
            items.push(self.b.get(a..b)?);
        }
        let end = data + self.offset(offs + count * size, size)?;
        Some((items, end))
    }
}

pub(crate) fn dict(b: &[u8]) -> Vec<(u16, Vec<f64>)> {
    let mut out = Vec::new();
    let mut ops: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let b0 = b[i];
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    i += 1;
                    1200 + u16::from(*b.get(i).unwrap_or(&0))
                } else {
                    u16::from(b0)
                };
                out.push((op, std::mem::take(&mut ops)));
                i += 1;
            }
            28 => {
                let v =
                    i16::from_be_bytes([*b.get(i + 1).unwrap_or(&0), *b.get(i + 2).unwrap_or(&0)]);
                ops.push(f64::from(v));
                i += 3;
            }
            29 => {
                let mut v = [0u8; 4];
                for (k, x) in v.iter_mut().enumerate() {
                    *x = *b.get(i + 1 + k).unwrap_or(&0);
                }
                ops.push(f64::from(i32::from_be_bytes(v)));
                i += 5;
            }
            30 => {
                i += 1;
                while i < b.len() {
                    let v = b[i];
                    i += 1;
                    if v & 0x0f == 0x0f || v >> 4 == 0x0f {
                        break;
                    }
                }
                ops.push(0.0);
            }
            32..=246 => {
                ops.push(f64::from(b0) - 139.0);
                i += 1;
            }
            247..=250 => {
                let b1 = f64::from(*b.get(i + 1).unwrap_or(&0));
                ops.push((f64::from(b0) - 247.0) * 256.0 + b1 + 108.0);
                i += 2;
            }
            251..=254 => {
                let b1 = f64::from(*b.get(i + 1).unwrap_or(&0));
                ops.push(-(f64::from(b0) - 251.0) * 256.0 - b1 - 108.0);
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

pub(crate) fn cff(b: &[u8]) -> Option<Vec<Option<String>>> {
    let r = Reader { b };
    let hdr = r.u8(2)?;
    let (_, after_name) = r.index(hdr)?;
    let (tops, after_top) = r.index(after_name)?;
    let (strings, _) = r.index(after_top)?;
    let top = dict(tops.first()?);
    let operand = |op: u16| {
        top.iter()
            .find(|(o, _)| *o == op)
            .and_then(|(_, v)| v.first().copied())
    };
    if top.iter().any(|(o, _)| *o == 1230) {
        return None;
    }
    let enc_off = operand(16).unwrap_or(0.0) as usize;
    if enc_off <= 1 {
        return None;
    }
    let charstrings = operand(17)? as usize;
    let n_glyphs = r.u16(charstrings)?;
    let charset_off = operand(15).unwrap_or(0.0) as usize;
    let name = |sid: usize| -> Option<String> {
        if sid < STANDARD_STRINGS.len() {
            Some(STANDARD_STRINGS[sid].to_string())
        } else {
            strings
                .get(sid - STANDARD_STRINGS.len())
                .map(|s| String::from_utf8_lossy(s).into_owned())
        }
    };
    // 省略 charset 就是预定义的 ISOAdobe 字符集，它描述的是 229 个字形的完整字体。
    // 子集字体省略 charset 是写入方的错误（实测 Exstream 生成的字体），字形名整体错位，不可信
    if charset_off == 0 && n_glyphs < ISO_ADOBE_GLYPHS {
        return None;
    }
    let mut sids = vec![0usize; n_glyphs];
    if charset_off == 0 {
        for (g, s) in sids.iter_mut().enumerate() {
            *s = g;
        }
    } else if charset_off > 2 {
        let format = r.u8(charset_off)?;
        let mut at = charset_off + 1;
        let mut g = 1;
        match format {
            0 => {
                while g < n_glyphs {
                    sids[g] = r.u16(at)?;
                    at += 2;
                    g += 1;
                }
            }
            1 | 2 => {
                while g < n_glyphs {
                    let first = r.u16(at)?;
                    let left = if format == 1 {
                        r.u8(at + 2)?
                    } else {
                        r.u16(at + 2)?
                    };
                    at += if format == 1 { 3 } else { 4 };
                    for k in 0..=left {
                        if g >= n_glyphs {
                            break;
                        }
                        sids[g] = first + k;
                        g += 1;
                    }
                }
            }
            _ => return None,
        }
    } else {
        return None;
    }
    let mut out: Vec<Option<String>> = vec![None; 256];
    let format = r.u8(enc_off)?;
    let at = enc_off + 1;
    // 补充表紧跟在编码数据之后
    let supplement = match format & 0x7f {
        0 => {
            let n = r.u8(at)?;
            for g in 1..=n {
                let code = r.u8(at + g)?;
                if let Some(&sid) = sids.get(g) {
                    out[code] = name(sid);
                }
            }
            at + 1 + n
        }
        1 => {
            let n = r.u8(at)?;
            let mut g = 1;
            for k in 0..n {
                let first = r.u8(at + 1 + 2 * k)?;
                let left = r.u8(at + 2 + 2 * k)?;
                for slot in out.iter_mut().take((first + left).min(255) + 1).skip(first) {
                    if let Some(&sid) = sids.get(g) {
                        *slot = name(sid);
                    }
                    g += 1;
                }
            }
            at + 1 + 2 * n
        }
        _ => return None,
    };
    if format & 0x80 != 0 {
        let n = r.u8(supplement)?;
        for k in 0..n {
            let code = r.u8(supplement + 1 + 3 * k)?;
            let sid = r.u16(supplement + 2 + 3 * k)?;
            out[code] = name(sid);
        }
    }
    Some(out)
}
