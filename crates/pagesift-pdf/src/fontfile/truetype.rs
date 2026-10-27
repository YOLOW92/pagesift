//! 嵌入 TrueType 字体（FontFile2）的 `cmap` 表：反查字形编号对应的 Unicode。
//!
//! 复合字体没有 ToUnicode、字体是 TrueType 时，内容流里的码经 CIDToGIDMap 变成字形编号；字体自带的 Unicode cmap
//! （平台 3 编码 1 或 10、平台 0）记着“码位 → 字形”，倒过来就是“字形 → 码位”（pdf.js、poppler 也这样兜底）。
//! 只用 Unicode 子表：符号子表（3,0）的码不是 Unicode。格式 4（BMP）和 12（全平面）。

use std::collections::HashMap;

fn u16_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?])))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

/// 字形编号 → 字符。读不出 Unicode 子表返回 None。一个字形对应多个码位时取最小的（常用字在前）。
pub(crate) fn glyph_unicode(font: &[u8]) -> Option<HashMap<u32, char>> {
    // 表目录：偏移 4 是表数，从 12 开始每条 16 字节（标签、校验和、偏移、长度）
    let tables = u16_at(font, 4)? as usize;
    let cmap = (0..tables).find_map(|k| {
        let e = 12 + 16 * k;
        (font.get(e..e + 4)? == b"cmap").then(|| u32_at(font, e + 8)).flatten()
    })? as usize;
    let subtables = u16_at(font, cmap + 2)? as usize;
    // 挑子表：全平面的格式 12 优先，其次 BMP 的格式 4
    let mut best: Option<(u8, usize)> = None;
    for k in 0..subtables {
        let e = cmap + 4 + 8 * k;
        let (platform, encoding) = (u16_at(font, e)?, u16_at(font, e + 2)?);
        let off = cmap + u32_at(font, e + 4)? as usize;
        let unicode = platform == 0 || (platform == 3 && matches!(encoding, 1 | 10));
        if !unicode {
            continue;
        }
        let rank = match u16_at(font, off)? {
            12 => 2,
            4 => 1,
            _ => continue,
        };
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, off));
        }
    }
    let (rank, off) = best?;
    let mut map: HashMap<u32, char> = HashMap::new();
    let mut put = |gid: u32, cp: u32| {
        if gid == 0 {
            return;
        }
        if let Some(c) = char::from_u32(cp) {
            map.entry(gid).and_modify(|e| {
                if c < *e {
                    *e = c;
                }
            }).or_insert(c);
        }
    };
    if rank == 2 {
        // 格式 12：分组 (起始码位, 结束码位, 起始字形)
        let groups = u32_at(font, off + 12)? as usize;
        for g in 0..groups.min(200_000) {
            let e = off + 16 + 12 * g;
            let (start, end, gid) = (u32_at(font, e)?, u32_at(font, e + 4)?, u32_at(font, e + 8)?);
            for cp in start..=end.min(start + 0xFFFF) {
                put(gid + (cp - start), cp);
            }
        }
    } else {
        // 格式 4：段 (endCode, startCode, idDelta, idRangeOffset)
        let segs = (u16_at(font, off + 6)? / 2) as usize;
        let ends = off + 14;
        let starts = ends + 2 * segs + 2;
        let deltas = starts + 2 * segs;
        let ranges = deltas + 2 * segs;
        for s in 0..segs {
            let (end, start) = (u16_at(font, ends + 2 * s)?, u16_at(font, starts + 2 * s)?);
            let delta = u16_at(font, deltas + 2 * s)?;
            let range = u16_at(font, ranges + 2 * s)? as usize;
            if start > end || start == 0xFFFF {
                continue;
            }
            for cp in start..=end {
                let gid = if range == 0 {
                    (cp + delta) & 0xFFFF
                } else {
                    let at = ranges + 2 * s + range + 2 * (cp - start) as usize;
                    match u16_at(font, at)? {
                        0 => 0,
                        g => (g + delta) & 0xFFFF,
                    }
                };
                put(gid, cp);
            }
        }
    }
    (!map.is_empty()).then_some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有 cmap 表的最小字体：格式 4，一段把 U+4E2D（中）映射到字形 7。
    fn tiny_font() -> Vec<u8> {
        let mut cmap = Vec::new();
        cmap.extend([0, 0, 0, 1]); // 版本、子表数
        cmap.extend([0, 3, 0, 1, 0, 0, 0, 12]); // (3,1) 偏移 12
        let segs: [(u16, u16, u16); 2] = [(0x4E2D, 0x4E2D, 7u16.wrapping_sub(0x4E2D)), (0xFFFF, 0xFFFF, 1)];
        let mut sub = vec![0, 4, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0];
        for s in segs {
            sub.extend(s.1.to_be_bytes());
        }
        sub.extend([0, 0]);
        for s in segs {
            sub.extend(s.0.to_be_bytes());
        }
        for s in segs {
            sub.extend(s.2.to_be_bytes());
        }
        sub.extend([0, 0, 0, 0]);
        cmap.extend(sub);
        let mut font = vec![0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        font.extend(b"cmap");
        font.extend([0, 0, 0, 0]);
        font.extend(28u32.to_be_bytes());
        font.extend((cmap.len() as u32).to_be_bytes());
        font.extend(cmap);
        font
    }

    #[test]
    fn format4_inverted() {
        let m = glyph_unicode(&tiny_font()).unwrap();
        assert_eq!(m.get(&7), Some(&'中'));
        assert_eq!(m.len(), 1);
    }
}
