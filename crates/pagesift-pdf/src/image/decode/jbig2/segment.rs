//! 段头（T.88 §7.2）和两种码流组织（附录 D：顺序、随机访问），以及长度未知的立即通用区域段
//! （§7.2.7）。PDF 嵌入的码流是不带文件头的顺序组织（ISO 32000-2 §7.4.7）；也认带文件头的独立文件。

use crate::image::jbig2::ID;

/// 一个段：段头里解码要用的字段和段数据。
#[derive(Debug, Clone)]
pub(super) struct Segment<'a> {
    pub(super) number: u32,
    pub(super) kind: u8,
    /// 引用的段号，按段头里的顺序。
    pub(super) refs: Vec<u32>,
    pub(super) data: &'a [u8],
}

/// [协议] 立即通用区域段（无损与否）的类型号（T.88 §7.3 表 2）
const IMMEDIATE_GENERIC: [u8; 2] = [38, 39];
/// [协议] 文件尾段的类型号
const END_OF_FILE: u8 = 51;

fn be32(b: &[u8], i: usize) -> Result<u32, String> {
    let s = b.get(i..i + 4).ok_or("段头被截断")?; // [协议] 4 字节大端整数
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// 读一个段头：返回段（数据先留空）、段头长度和数据长度（None 是未知）。
fn header<'a>(b: &[u8]) -> Result<(Segment<'a>, usize, Option<usize>), String> {
    let number = be32(b, 0)?;
    let flags = *b.get(4).ok_or("段头被截断")?; // [协议] 段号之后是 1 字节标志（§7.2.3）
    // [协议] 低 6 位是类型，位 6 表示页关联字段占 4 字节
    let kind = flags & 0x3F;
    let wide_page = flags & 0x40 != 0; // [协议] 位 6（§7.2.3）
    let mut i = 5; // [协议] 段号 4 字节 + 标志 1 字节
    let first = *b.get(i).ok_or("段头被截断")?;
    // [协议] 引用段数（§7.2.4）：高 3 位；为 7 时是长格式，4 字节里的低 29 位，后跟保留位
    let count = if first >> 5 == 7 {
        let n = (be32(b, i)? & 0x1FFF_FFFF) as usize; // [协议] 长格式：4 字节里的低 29 位
        // [协议] 4 字节计数之后，本段和每个被引用段各一个保留位，按字节补齐（§7.2.4）
        i += 4 + (n + 1).div_ceil(8);
        n
    } else {
        i += 1;
        usize::from(first >> 5) // [协议] 短格式：高 3 位
    };
    let width = match number {
        // [协议] 被引用段号的宽度由本段号决定：≤256 一字节，≤65536 两字节，否则四字节（§7.2.5）
        0..=256 => 1,
        257..=65_536 => 2,
        _ => 4,
    };
    let need = count.checked_mul(width).ok_or("引用段数溢出")?;
    let list = b.get(i..i + need).ok_or("段头被截断")?;
    // [协议] 被引用段号按大端存放，每字节 8 位（§7.2.5）
    let refs = list
        .chunks(width)
        .map(|c| c.iter().fold(0u32, |v, &x| (v << 8) | u32::from(x)))
        .collect();
    i += need;
    i += if wide_page { 4 } else { 1 }; // [协议] 页关联字段 1 或 4 字节（§7.2.6）
    let len = be32(b, i)?;
    i += 4; // [协议] 数据长度 4 字节（§7.2.7）
    let len = (len != u32::MAX).then_some(len as usize);
    let seg = Segment {
        number,
        kind,
        refs,
        data: &[],
    };
    Ok((seg, i, len))
}

/// 长度未知的立即通用区域段（§7.2.7）：数据以结束标记加 4 字节行数收尾。
/// MMR 编码时标记是 0x0000，算术编码时是 0xFFAC；从区域段信息和标志之后开始找。
fn unknown_length(b: &[u8]) -> Result<usize, String> {
    // [协议] 区域段信息 17 字节，其后 1 字节通用区域标志，位 0 是 MMR（§7.4.1、§7.4.6.2）
    let flags = *b.get(17).ok_or("通用区域段太短")?;
    // [协议] 结束标记：MMR 是 0x0000，算术编码是 0xFFAC（§7.2.7）
    let marker: [u8; 2] = if flags & 1 != 0 { [0, 0] } else { [0xFF, 0xAC] };
    let from = 18; // [协议] 区域段信息 17 字节 + 标志 1 字节
    let at = b
        .get(from..)
        .and_then(|r| r.windows(2).position(|w| w == marker))
        .ok_or("长度未知的通用区域段没有结束标记")?;
    // [协议] 标记 2 字节 + 行数 4 字节
    let end = from + at + 2 + 4;
    if end > b.len() {
        return Err("长度未知的通用区域段被截断".into());
    }
    Ok(end)
}

/// 把码流拆成段。认三种：PDF 嵌入（无文件头的顺序组织）、带文件头的顺序组织、随机访问组织。
pub(super) fn parse(data: &[u8]) -> Result<Vec<Segment<'_>>, String> {
    let mut i = 0;
    let mut random = false;
    if data.starts_with(&ID) {
        // [协议] 文件头：ID 串 8 字节、标志 1 字节；标志位 0 为 1 是顺序组织，位 1 为 0 时跟 4 字节页数（附录 D.4）
        let flags = *data.get(8).ok_or("文件头被截断")?;
        random = flags & 1 == 0;
        i = if flags & 2 == 0 { 13 } else { 9 }; // [协议] 文件头 9 字节，跟页数时再加 4 字节
    }
    let mut out = Vec::new();
    if random {
        // 先是全部段头，直到文件尾段；数据按同样顺序接在后面
        let mut heads = Vec::new();
        while i < data.len() {
            let (seg, hl, len) = header(&data[i..])?;
            i += hl;
            let len = len.ok_or("随机访问组织里不允许长度未知的段")?;
            let end = seg.kind == END_OF_FILE;
            heads.push((seg, len));
            if end {
                break;
            }
        }
        for (mut seg, len) in heads {
            seg.data = data.get(i..i + len).ok_or("段数据被截断")?;
            i += len;
            out.push(seg);
        }
        return Ok(out);
    }
    while i < data.len() {
        let (mut seg, hl, len) = header(&data[i..])?;
        i += hl;
        let rest = &data[i..];
        let len = match len {
            Some(n) => n,
            None if IMMEDIATE_GENERIC.contains(&seg.kind) => unknown_length(rest)?,
            None => return Err(format!("类型 {} 的段长度未知", seg.kind)),
        };
        // 最后一段被截断时有多少用多少（PDF 里常见，区域解码会在数据末尾停下）
        let len = len.min(rest.len());
        seg.data = &rest[..len];
        i += len;
        let end = seg.kind == END_OF_FILE;
        out.push(seg);
        if end {
            break;
        }
    }
    Ok(out)
}
