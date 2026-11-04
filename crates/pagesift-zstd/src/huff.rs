//! Huffman 字面量：权重表解析、解码表、1 流/4 流解码。
use crate::bits::RevBits;
use crate::fse;
use crate::{Error, Result};

#[derive(Clone)]
pub(crate) struct HuffTable {
    pub max_bits: u32,
    /// 下标是 max_bits 位前缀，值是（符号，码长）。
    pub entries: Vec<(u8, u8)>,
}

/// 解析树描述，返回（表，消耗字节）。
pub(crate) fn read_tree(data: &[u8]) -> Result<(HuffTable, usize)> {
    let hb = *data.first().ok_or(Error::Corrupt("empty huffman header"))? as usize;
    let mut weights: Vec<u8> = Vec::new();
    let used = if hb >= 128 {
        let n = hb - 127;
        let bytes = n.div_ceil(2);
        let body = data
            .get(1..1 + bytes)
            .ok_or(Error::Corrupt("huffman weights truncated"))?;
        for i in 0..n {
            let b = body[i / 2];
            weights.push(if i % 2 == 0 { b >> 4 } else { b & 15 });
        }
        1 + bytes
    } else {
        let body = data
            .get(1..1 + hb)
            .ok_or(Error::Corrupt("huffman fse truncated"))?;
        let (t, off) = fse::read_table(body, 255, 6)?;
        let mut r = RevBits::new(body.get(off..).ok_or(Error::Corrupt("huffman fse"))?)?;
        let mut s1 = r.read(t.al) as usize;
        let mut s2 = r.read(t.al) as usize;
        loop {
            if weights.len() > 255 {
                return Err(Error::Corrupt("too many huffman weights"));
            }
            let c = t.cells[s1];
            weights.push(c.sym);
            s1 = c.base as usize + r.read(c.nb as u32) as usize;
            if r.overflow() {
                weights.push(t.cells[s2].sym);
                break;
            }
            let c = t.cells[s2];
            weights.push(c.sym);
            s2 = c.base as usize + r.read(c.nb as u32) as usize;
            if r.overflow() {
                weights.push(t.cells[s1].sym);
                break;
            }
        }
        1 + hb
    };
    if weights.len() > 255 {
        return Err(Error::Corrupt("too many huffman weights"));
    }
    let mut sum: u32 = 0;
    for &w in &weights {
        if w > 11 {
            return Err(Error::Corrupt("huffman weight too large"));
        }
        if w > 0 {
            sum += 1 << (w - 1);
        }
    }
    if sum == 0 {
        return Err(Error::Corrupt("empty huffman tree"));
    }
    let max_bits = 32 - sum.leading_zeros(); // 向上到 2 的幂
    if max_bits > 11 {
        return Err(Error::Corrupt("huffman depth"));
    }
    let rest = (1u32 << max_bits) - sum;
    if rest == 0 || !rest.is_power_of_two() {
        return Err(Error::Corrupt("huffman weights do not complete"));
    }
    weights.push((rest.trailing_zeros() + 1) as u8);
    let mut entries = vec![(0u8, 0u8); 1 << max_bits];
    let mut pos = 0usize;
    for w in 1..=max_bits as u8 {
        for (sym, &sw) in weights.iter().enumerate() {
            if sw == w {
                let len = 1usize << (w - 1);
                let nb = (max_bits as u8) + 1 - w;
                for e in &mut entries[pos..pos + len] {
                    *e = (sym as u8, nb);
                }
                pos += len;
            }
        }
    }
    Ok((HuffTable { max_bits, entries }, used))
}

fn decode_stream(t: &HuffTable, data: &[u8], out: &mut [u8]) -> Result<()> {
    let mut r = RevBits::new(data)?;
    for o in out.iter_mut() {
        let (sym, nb) = t.entries[r.peek(t.max_bits) as usize];
        r.skip(nb as u32);
        *o = sym;
    }
    if r.remaining() != 0 {
        return Err(Error::Corrupt("huffman stream length"));
    }
    Ok(())
}

pub(crate) fn decode(t: &HuffTable, data: &[u8], four: bool, n: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; n];
    if !four {
        decode_stream(t, data, &mut out)?;
        return Ok(out);
    }
    let jt = data.get(..6).ok_or(Error::Corrupt("huffman jump table"))?;
    let s1 = u16::from_le_bytes([jt[0], jt[1]]) as usize;
    let s2 = u16::from_le_bytes([jt[2], jt[3]]) as usize;
    let s3 = u16::from_le_bytes([jt[4], jt[5]]) as usize;
    let body = &data[6..];
    if s1 + s2 + s3 > body.len() {
        return Err(Error::Corrupt("huffman stream sizes"));
    }
    let seg = n.div_ceil(4);
    if seg * 3 > n {
        return Err(Error::Corrupt("huffman 4-stream size"));
    }
    let parts = [
        &body[..s1],
        &body[s1..s1 + s2],
        &body[s1 + s2..s1 + s2 + s3],
        &body[s1 + s2 + s3..],
    ];
    let (a, rest) = out.split_at_mut(seg);
    let (b, rest) = rest.split_at_mut(seg);
    let (c, d) = rest.split_at_mut(seg);
    decode_stream(t, parts[0], a)?;
    decode_stream(t, parts[1], b)?;
    decode_stream(t, parts[2], c)?;
    decode_stream(t, parts[3], d)?;
    Ok(out)
}
