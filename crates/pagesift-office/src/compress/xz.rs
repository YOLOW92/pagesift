//! `.xz` 容器（xz 文件格式 1.2）：只支持单个 LZMA2 过滤器。BCJ 这类过滤器报不支持。
//! 校验值（CRC、SHA-256）不验证：内容坏了 LZMA 自己大多会报错，这里只是读文字。

use super::lzma;
use crate::Error;

pub(crate) const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0];
/// 各校验类型的字节数（xz 格式 §2.1.1.2）。
const CHECK_SIZES: [usize; 16] = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64];

fn bad(m: &str) -> Error {
    Error::Malformed(format!("xz: {m}"))
}

fn varint(b: &[u8], pos: &mut usize) -> Result<u64, Error> {
    let mut out = 0u64;
    for i in 0..9 {
        let v = *b.get(*pos).ok_or_else(|| bad("truncated integer"))?;
        *pos += 1;
        out |= u64::from(v & 0x7F) << (i * 7);
        if v & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err(bad("integer too long"))
}

pub(crate) fn decode(input: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        pos = stream(input, pos, &mut out, limit)?;
        // 流之间可以有 4 字节对齐的零填充
        while input.get(pos) == Some(&0) {
            pos += 1;
        }
        if !input[pos.min(input.len())..].starts_with(&MAGIC) {
            return Ok(out);
        }
    }
}

fn stream(input: &[u8], start: usize, out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
    let header = input
        .get(start..start + 12)
        .ok_or_else(|| bad("truncated header"))?;
    if !header.starts_with(&MAGIC) {
        return Err(bad("wrong signature"));
    }
    let check = CHECK_SIZES[usize::from(header[7] & 0x0F)];
    let mut pos = start + 12;
    loop {
        let size_byte = *input.get(pos).ok_or_else(|| bad("truncated block"))?;
        if size_byte == 0 {
            return index_and_footer(input, pos);
        }
        let header_size = (usize::from(size_byte) + 1) * 4;
        let block = input
            .get(pos..pos + header_size)
            .ok_or_else(|| bad("truncated block header"))?;
        let flags = block[1];
        let mut p = 2usize;
        if flags & 0x40 != 0 {
            varint(block, &mut p)?;
        }
        if flags & 0x80 != 0 {
            varint(block, &mut p)?;
        }
        let filters = usize::from(flags & 3) + 1;
        for _ in 0..filters {
            let id = varint(block, &mut p)?;
            let props = varint(block, &mut p)? as usize;
            p = p.saturating_add(props);
            if id != 0x21 || filters != 1 {
                return Err(Error::Unsupported(format!("xz filter {id:#x}")));
            }
        }
        let data_start = pos + header_size;
        let data = input
            .get(data_start..)
            .ok_or_else(|| bad("truncated block"))?;
        let used = lzma::decode_lzma2(data, out, limit)?;
        // 压缩数据补齐到 4 字节，后面跟校验值
        pos = data_start + used.div_ceil(4) * 4 + check;
    }
}

fn index_and_footer(input: &[u8], start: usize) -> Result<usize, Error> {
    let mut p = start + 1;
    let records = varint(input, &mut p)?;
    if records > input.len() as u64 {
        return Err(bad("index is too long"));
    }
    for _ in 0..records {
        varint(input, &mut p)?;
        varint(input, &mut p)?;
    }
    // 索引补齐到 4 字节，加 4 字节 CRC，再加 12 字节流尾
    let index_len = (p - start).div_ceil(4) * 4;
    Ok((start + index_len + 4 + 12).min(input.len()))
}
