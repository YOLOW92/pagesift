//! PDF 里的 JBIG2 码流拼成独立的 JBIG2 文件（ITU-T T.88 附录 D.1 顺序组织）。
//!
//! PDF 嵌入的码流不带文件头、页尾段和文件尾段（ISO 32000-2 §7.4.7），全局段放在单独的
//! /JBIG2Globals 流里。独立文件 = 文件头（ID 串 + 标志 + 页数 1）、全局段、本页的段、
//! 页尾段（类型 49）、文件尾段（类型 51）依次相接。补上的两段编号接在已有段号的最大值后面。
//! 拼之前逐段读段头（T.88 §7.2），段结构不对就不导出。

/// [协议] 文件头的 ID 串（T.88 §D.4.1）。
pub(crate) const ID: [u8; 8] = [0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A];
/// [协议] 段类型：页信息 48、页尾 49、条带尾 50、文件尾 51（T.88 §7.3 表 2）。
pub(crate) const PAGE_INFO: u8 = 48;
pub(crate) const END_OF_PAGE: u8 = 49; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
pub(crate) const END_OF_FILE: u8 = 51; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）

/// 读出的一个段头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) number: u32,
    pub(crate) kind: u8,
    pub(crate) page: u32,
    /// 段头 + 段数据在码流里的范围。
    pub(crate) start: usize,
    pub(crate) end: usize,
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?)) // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
}

/// 逐段读顺序组织的码流（T.88 §7.2）。段头或数据长度越界返回 None。
pub(crate) fn segments(data: &[u8]) -> Option<Vec<Segment>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let start = i;
        let number = be32(data, i)?;
        let flags = *data.get(i + 4)?; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        // [协议] 段头标志：低 6 位是类型，位 6 表示页关联字段占 4 字节（§7.2.3）
        let kind = flags & 0x3F;
        let wide_page = flags & 0x40 != 0; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        i += 5; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        // 引用段数和保留标志（§7.2.4）：短格式 1 字节，高 3 位是个数；个数为 7 时是长格式
        let first = *data.get(i)?;
        // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        let count = if first >> 5 == 7 {
            // [协议] 长格式：4 字节，低 29 位是个数，后跟 ⌈(个数+1)/8⌉ 字节的保留位
            let n = be32(data, i)? & 0x1FFF_FFFF;
            i += 4 + (n as usize + 1).div_ceil(8); // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
            n as usize
        } else {
            i += 1;
            usize::from(first >> 5) // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        };
        // [协议] 被引用段号的宽度由本段号决定：≤256 一字节，≤65536 两字节，否则四字节（§7.2.5）
        let width = if number <= 256 {
            1
            // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        } else if number <= 65_536 {
            2
        } else {
            4 // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        };
        i = i.checked_add(count.checked_mul(width)?)?;
        let page = if wide_page {
            let p = be32(data, i)?;
            i += 4; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
            p
        } else {
            let p = u32::from(*data.get(i)?);
            i += 1;
            p
        };
        let len = be32(data, i)?;
        i += 4; // [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
        // 长度未知（0xFFFFFFFF，只有立即通用区域段允许，§7.2.7）：当作码流里的最后一段
        let end = if len == u32::MAX {
            data.len()
        } else {
            i.checked_add(len as usize).filter(|&e| e <= data.len())?
        };
        out.push(Segment {
            number,
            kind,
            page,
            start,
            end,
        });
        i = end;
    }
    Some(out)
}

/// 一个没有数据、不引用别的段的段头（短格式、1 字节页关联）。
// [协议] JBIG2 段头布局（ITU-T T.88 §7.2）
fn bare_segment(number: u32, kind: u8, page: u8) -> [u8; 11] {
    let n = number.to_be_bytes();
    [n[0], n[1], n[2], n[3], kind, 0, page, 0, 0, 0, 0]
}

/// 拼成独立文件。`globals` 是 /JBIG2Globals 流解码后的数据（没有时为空）。
pub(crate) fn wrap(globals: &[u8], page: &[u8]) -> Result<Vec<u8>, String> {
    let g = segments(globals).ok_or("JBIG2Globals 的段结构不完整")?;
    let p = segments(page).ok_or("JBIG2 码流的段结构不完整")?;
    if !p.iter().any(|s| s.kind == PAGE_INFO) {
        return Err("JBIG2 码流没有页信息段".into());
    }
    let mut next = g.iter().chain(&p).map(|s| s.number).max().unwrap_or(0);
    let mut out = ID.to_vec();
    // [协议] 文件头标志：位 0 = 1 顺序组织，位 1 = 0 页数已知；随后 4 字节页数（§D.4.2、§D.4.3）
    out.push(0x01);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(globals);
    out.extend_from_slice(page);
    if !p.iter().any(|s| s.kind == END_OF_PAGE) {
        next += 1;
        out.extend_from_slice(&bare_segment(next, END_OF_PAGE, 1));
    }
    next += 1;
    out.extend_from_slice(&bare_segment(next, END_OF_FILE, 0));
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 造一个段：短格式头、1 字节页关联。
    pub(crate) fn seg(number: u32, kind: u8, page: u8, refs: &[u8], data: &[u8]) -> Vec<u8> {
        let mut v = number.to_be_bytes().to_vec();
        v.push(kind);
        v.push((refs.len() as u8) << 5);
        v.extend_from_slice(refs);
        v.push(page);
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn reads_segment_headers() {
        let mut s = seg(0, 0, 0, &[], &[1, 2, 3]); // 符号字典（全局）
        s.extend(seg(1, PAGE_INFO, 1, &[], &[0; 19]));
        s.extend(seg(2, 6, 1, &[0], &[9, 9])); // 引用段 0 的文本区域
        let v = segments(&s).unwrap();
        assert_eq!(
            v.iter()
                .map(|x| (x.number, x.kind, x.page))
                .collect::<Vec<_>>(),
            [(0, 0, 0), (1, PAGE_INFO, 1), (2, 6, 1)]
        );
        assert_eq!(v[2].end, s.len());
        // 数据长度越界
        assert_eq!(segments(&s[..s.len() - 1]), None);
    }

    #[test]
    fn wrapped_file_has_header_and_end_segments() {
        let globals = seg(0, 0, 0, &[], &[7]);
        let mut page = seg(1, PAGE_INFO, 1, &[], &[0; 19]);
        page.extend(seg(2, 38, 1, &[], &[5, 5, 5]));
        let f = wrap(&globals, &page).unwrap();
        assert_eq!(&f[..8], &ID);
        assert_eq!(f[8], 1);
        assert_eq!(&f[9..13], &[0, 0, 0, 1]);
        let v = segments(&f[13..]).unwrap();
        let kinds: Vec<u8> = v.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [0, PAGE_INFO, 38, END_OF_PAGE, END_OF_FILE]);
        assert_eq!(v[3].number, 3);
        assert_eq!(v[4].number, 4);
        assert_eq!((v[3].page, v[4].page), (1, 0));
        // 没有页信息段的码流不导出
        assert!(wrap(&[], &seg(1, 38, 1, &[], &[1])).is_err());
    }
}
