//! Huffman 字面量：权重表解析、解码表、1 流/4 流解码。
use crate::bits::RevBits;
use crate::fse;
use crate::{Error, Result};

#[derive(Clone)]
pub(crate) struct HuffTable {
    max_bits: u32,
    entries: Vec<u16>,
    pairs: Option<PairTable>,
}

#[derive(Clone)]
struct PairTable {
    lookup_bits: u32,
    /// 低两字节为输出符号，高两字节为总码长、首符号码长。
    entries: Vec<u32>,
}

/// 解析权重及隐含的最后一个符号，返回（权重，最大码长，消耗字节）。
fn read_weights(data: &[u8]) -> Result<(Vec<u8>, u32, usize)> {
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
    Ok((weights, max_bits, used))
}

pub(crate) fn check_tree(data: &[u8]) -> Result<(u32, usize)> {
    read_weights(data).map(|(_, bits, used)| (bits, used))
}

pub(crate) fn read_tree(data: &[u8]) -> Result<(HuffTable, usize)> {
    let (weights, max_bits, used) = read_weights(data)?;
    let mut entries = vec![0u16; 1 << max_bits];
    let mut pos = 0usize;
    for w in 1..=max_bits as u8 {
        for (sym, &sw) in weights.iter().enumerate() {
            if sw == w {
                let len = 1usize << (w - 1);
                let nb = (max_bits as u8) + 1 - w;
                for e in &mut entries[pos..pos + len] {
                    *e = (sym as u16) << 8 | nb as u16;
                }
                pos += len;
            }
        }
    }
    Ok((
        HuffTable {
            max_bits,
            entries,
            pairs: None,
        },
        used,
    ))
}

fn build_pairs(entries: &[u16], max_bits: u32, lookup_bits: u32) -> PairTable {
    let mask = (1usize << lookup_bits) - 1;
    let shift = lookup_bits - max_bits;
    let pairs = (0..=mask)
        .map(|prefix| {
            let first = entries[prefix >> shift];
            let n1 = (first & 255) as u32;
            let second = entries[((prefix << n1) & mask) >> shift];
            let n2 = (second & 255) as u32;
            let (symbol, bits) = if n1 + n2 <= lookup_bits {
                ((second >> 8) as u32, n1 + n2)
            } else {
                (0, n1)
            };
            (first >> 8) as u32 | (symbol << 8) | (bits << 16) | (n1 << 24)
        })
        .collect();
    PairTable {
        lookup_bits,
        entries: pairs,
    }
}

fn decode_pair_stream(t: &PairTable, data: &[u8], out: &mut [u8]) -> Result<()> {
    let mut r = RevBits::new(data)?;
    finish_stream(t, &mut r, out, 0)
}

#[inline(always)]
fn pair(t: &PairTable, r: &mut RevBits<'_>, out: &mut [u8], pos: usize) -> usize {
    let e = t.entries[r.peek(t.lookup_bits) as usize];
    let bits = (e >> 16) & 255;
    r.skip(bits);
    // 单符号条目的第二字节是暂写值，下次迭代覆盖；调用方保证还有两个输出位置。
    out[pos] = e as u8;
    out[pos + 1] = (e >> 8) as u8;
    pos + 1 + usize::from(bits > e >> 24)
}

fn finish_stream(t: &PairTable, r: &mut RevBits<'_>, out: &mut [u8], mut pos: usize) -> Result<()> {
    while pos + 1 < out.len() {
        pos = pair(t, r, out, pos);
    }
    if pos < out.len() {
        let e = t.entries[r.peek(t.lookup_bits) as usize];
        r.skip(e >> 24);
        out[pos] = e as u8;
    }
    if r.remaining() != 0 {
        return Err(Error::Corrupt("huffman stream length"));
    }
    Ok(())
}

fn stream_parts(data: &[u8], n: usize) -> Result<([&[u8]; 4], usize)> {
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
    Ok((parts, seg))
}

fn decode_pairs(t: &PairTable, data: &[u8], four: bool, out: &mut [u8]) -> Result<()> {
    if !four {
        return decode_pair_stream(t, data, out);
    }
    let (parts, seg) = stream_parts(data, out.len())?;
    let [mut a, mut b, mut c, mut d] = [
        RevBits::new(parts[0])?,
        RevBits::new(parts[1])?,
        RevBits::new(parts[2])?,
        RevBits::new(parts[3])?,
    ];
    let (oa, rest) = out.split_at_mut(seg);
    let (ob, rest) = rest.split_at_mut(seg);
    let (oc, od) = rest.split_at_mut(seg);
    // 交错处理四条独立位流，打断逐符号依赖链；无需 SIMD 或 unsafe。
    let (mut pa, mut pb, mut pc, mut pd) = (0, 0, 0, 0);
    while pa + 1 < oa.len() && pb + 1 < ob.len() && pc + 1 < oc.len() && pd + 1 < od.len() {
        pa = pair(t, &mut a, oa, pa);
        pb = pair(t, &mut b, ob, pb);
        pc = pair(t, &mut c, oc, pc);
        pd = pair(t, &mut d, od, pd);
    }
    finish_stream(t, &mut a, oa, pa)?;
    finish_stream(t, &mut b, ob, pb)?;
    finish_stream(t, &mut c, oc, pc)?;
    finish_stream(t, &mut d, od, pd)?;
    Ok(())
}

fn decode_single(t: &HuffTable, data: &[u8], four: bool, out: &mut [u8]) -> Result<()> {
    let symbol = |r: &mut RevBits<'_>| {
        let e = t.entries[r.peek(t.max_bits) as usize];
        r.skip((e & 255) as u32);
        (e >> 8) as u8
    };
    if !four {
        let mut r = RevBits::new(data)?;
        for o in out {
            *o = symbol(&mut r);
        }
        return if r.remaining() == 0 {
            Ok(())
        } else {
            Err(Error::Corrupt("huffman stream length"))
        };
    }
    let (parts, seg) = stream_parts(data, out.len())?;
    let [mut a, mut b, mut c, mut d] = [
        RevBits::new(parts[0])?,
        RevBits::new(parts[1])?,
        RevBits::new(parts[2])?,
        RevBits::new(parts[3])?,
    ];
    let (oa, rest) = out.split_at_mut(seg);
    let (ob, rest) = rest.split_at_mut(seg);
    let (oc, od) = rest.split_at_mut(seg);
    for i in 0..od.len() {
        oa[i] = symbol(&mut a);
        ob[i] = symbol(&mut b);
        oc[i] = symbol(&mut c);
        od[i] = symbol(&mut d);
    }
    for i in od.len()..seg {
        oa[i] = symbol(&mut a);
        ob[i] = symbol(&mut b);
        oc[i] = symbol(&mut c);
    }
    if [a, b, c, d].iter().any(|r| r.remaining() != 0) {
        return Err(Error::Corrupt("huffman stream length"));
    }
    Ok(())
}

pub(crate) fn decode_into(
    t: &mut HuffTable,
    data: &[u8],
    four: bool,
    out: &mut [u8],
) -> Result<()> {
    let lookup_bits = crate::resources::huffman_lookup_bits(t.max_bits);
    // 表项至少摊销一次双符号查询；小字面量块继续用原来的紧凑单符号表。
    if out.len() / 2 >= 1usize << lookup_bits {
        let pairs = t
            .pairs
            .get_or_insert_with(|| build_pairs(&t.entries, t.max_bits, lookup_bits));
        decode_pairs(pairs, data, four, out)
    } else {
        decode_single(t, data, four, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_lookup_handles_stream_tails() {
        let mut checked = 0;
        for n in [
            32, 33, 34, 35, 63, 64, 65, 127, 128, 129, 255, 256, 257, 258, 259, 260, 511, 512, 513,
            1023, 1024, 1025, 4095, 4096, 4097,
        ] {
            for seed in 1..=3 {
                let mut state = seed * 0x123456789u64;
                let data: Vec<u8> = (0..n)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        let x = (state >> 32) as u32 & 4095;
                        match seed {
                            1 => (x % 5) as u8,
                            2 => ((x * x / 4096) / 16) as u8,
                            _ => ((x % 31).min(7)) as u8,
                        }
                    })
                    .collect();
                let Some(encoded) = crate::henc::encode_literals(&data) else {
                    continue;
                };
                let sf = (encoded[0] >> 2) & 3;
                let header = match sf {
                    0 | 1 => 3,
                    2 => 4,
                    _ => 5,
                };
                let (mut tree, used) = read_tree(&encoded[header..]).unwrap();
                let mut output = vec![0; n];
                decode_into(&mut tree, &encoded[header + used..], sf != 0, &mut output).unwrap();
                assert_eq!(output, data, "n={n} seed={seed}");
                let pairs = build_pairs(
                    &tree.entries,
                    tree.max_bits,
                    crate::resources::huffman_lookup_bits(tree.max_bits),
                );
                decode_pairs(&pairs, &encoded[header + used..], sf != 0, &mut output).unwrap();
                assert_eq!(output, data, "paired n={n} seed={seed}");
                checked += 1;
            }
        }
        assert!(checked > 30);
    }
}
