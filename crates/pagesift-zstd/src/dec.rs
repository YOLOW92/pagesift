//! 帧、块、字面量段、序列段。
use crate::bits::RevBits;
use crate::fse::{self, FseTable};
use crate::huff::{self, HuffTable};
use crate::{Error, Result};

const MAX_BLOCK: usize = 128 * 1024;

const LL_BASE: [(u32, u32); 36] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 1),
    (18, 1),
    (20, 1),
    (22, 1),
    (24, 2),
    (28, 2),
    (32, 3),
    (40, 3),
    (48, 4),
    (64, 6),
    (128, 7),
    (256, 8),
    (512, 9),
    (1024, 10),
    (2048, 11),
    (4096, 12),
    (8192, 13),
    (16384, 14),
    (32768, 15),
    (65536, 16),
];
const ML_BASE: [(u32, u32); 53] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 0),
    (17, 0),
    (18, 0),
    (19, 0),
    (20, 0),
    (21, 0),
    (22, 0),
    (23, 0),
    (24, 0),
    (25, 0),
    (26, 0),
    (27, 0),
    (28, 0),
    (29, 0),
    (30, 0),
    (31, 0),
    (32, 0),
    (33, 0),
    (34, 0),
    (35, 1),
    (37, 1),
    (39, 1),
    (41, 1),
    (43, 2),
    (47, 2),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 5),
    (131, 7),
    (259, 8),
    (515, 9),
    (1027, 10),
    (2051, 11),
    (4099, 12),
    (8195, 13),
    (16387, 14),
    (32771, 15),
    (65539, 16),
];

pub(crate) const LL_DEFAULT: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
pub(crate) const ML_DEFAULT: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
pub(crate) const OF_DEFAULT: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

struct State {
    hist: [usize; 3],
    huff: Option<HuffTable>,
    ll: Option<FseTable>,
    of: Option<FseTable>,
    ml: Option<FseTable>,
}

/// 解压 `data`（可含多帧、跳过帧），输出不超过 `max_out` 字节。
pub fn decompress(data: &[u8], max_out: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    decompress_into(data, &mut out, max_out)?;
    Ok(out)
}

/// 同 [`decompress`]，但写入调用方的缓冲；出错时 `out` 里留着已解好的完整块（超限时按块截断）。
pub fn decompress_into(data: &[u8], out: &mut Vec<u8>, max_out: usize) -> Result<()> {
    let mut pos = 0usize;
    if data.is_empty() {
        return Err(Error::Corrupt("empty input"));
    }
    while pos < data.len() {
        let magic = data
            .get(pos..pos + 4)
            .ok_or(Error::Corrupt("truncated magic"))?;
        let m = u32::from_le_bytes([magic[0], magic[1], magic[2], magic[3]]);
        pos += 4;
        if (0x184D2A50..=0x184D2A5F).contains(&m) {
            let sz = data
                .get(pos..pos + 4)
                .ok_or(Error::Corrupt("truncated skip frame"))?;
            let n = u32::from_le_bytes([sz[0], sz[1], sz[2], sz[3]]) as usize;
            pos = pos
                .checked_add(4 + n)
                .filter(|&p| p <= data.len())
                .ok_or(Error::Corrupt("skip frame size"))?;
            continue;
        }
        if m != 0xFD2FB528 {
            return Err(Error::Corrupt("bad magic"));
        }
        pos += frame(&data[pos..], out, max_out)?;
    }
    Ok(())
}

/// 解一帧（不含魔数），返回消耗的字节数。
fn frame(d: &[u8], out: &mut Vec<u8>, max_out: usize) -> Result<usize> {
    let fd = *d.first().ok_or(Error::Corrupt("truncated frame header"))?;
    let single = fd & 0x20 != 0;
    if fd & 0x08 != 0 {
        return Err(Error::Corrupt("reserved bit set"));
    }
    let checksum = fd & 0x04 != 0;
    let did = [0usize, 1, 2, 4][(fd & 3) as usize];
    let fcs = match fd >> 6 {
        0 => single as usize,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let mut p = 1 + usize::from(!single);
    let hdr = d
        .get(p..p + did + fcs)
        .ok_or(Error::Corrupt("truncated frame header"))?;
    if hdr[..did].iter().any(|&b| b != 0) {
        return Err(Error::Unsupported("dictionary"));
    }
    if fcs > 0 {
        let mut v = 0u64;
        for (i, &b) in hdr[did..].iter().enumerate() {
            v |= (b as u64) << (8 * i);
        }
        if fcs == 2 {
            v += 256;
        }
        if v > (max_out.saturating_sub(out.len())) as u64 {
            return Err(Error::TooLarge);
        }
    }
    p += did + fcs;
    let start = out.len();
    let mut st = State {
        hist: [1, 4, 8],
        huff: None,
        ll: None,
        of: None,
        ml: None,
    };
    loop {
        let h = d
            .get(p..p + 3)
            .ok_or(Error::Corrupt("truncated block header"))?;
        let v = h[0] as usize | (h[1] as usize) << 8 | (h[2] as usize) << 16;
        p += 3;
        let last = v & 1 != 0;
        let size = v >> 3;
        match (v >> 1) & 3 {
            0 => {
                if size > MAX_BLOCK {
                    return Err(Error::Corrupt("raw block too large"));
                }
                let b = d
                    .get(p..p + size)
                    .ok_or(Error::Corrupt("truncated raw block"))?;
                push(out, b, start, max_out)?;
                p += size;
            }
            1 => {
                if size > MAX_BLOCK {
                    return Err(Error::Corrupt("rle block too large"));
                }
                let b = *d.get(p).ok_or(Error::Corrupt("truncated rle block"))?;
                if out.len() + size > max_out {
                    return Err(Error::TooLarge);
                }
                out.resize(out.len() + size, b);
                p += 1;
            }
            2 => {
                if size > MAX_BLOCK {
                    return Err(Error::Corrupt("block too large"));
                }
                let b = d
                    .get(p..p + size)
                    .ok_or(Error::Corrupt("truncated block"))?;
                block(b, &mut st, out, start, max_out)?;
                p += size;
            }
            _ => return Err(Error::Corrupt("reserved block type")),
        }
        if last {
            break;
        }
    }
    if checksum {
        let c = d
            .get(p..p + 4)
            .ok_or(Error::Corrupt("truncated checksum"))?;
        let want = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        if crate::xxh::xxh64(&out[start..]) as u32 != want {
            return Err(Error::Corrupt("checksum mismatch"));
        }
        p += 4;
    }
    Ok(p)
}

fn push(out: &mut Vec<u8>, b: &[u8], _start: usize, max_out: usize) -> Result<()> {
    if out.len() + b.len() > max_out {
        return Err(Error::TooLarge);
    }
    out.extend_from_slice(b);
    Ok(())
}

fn literals(b: &[u8], st: &mut State) -> Result<(Vec<u8>, usize)> {
    let b0 = b[0];
    let ty = b0 & 3;
    let sf = (b0 >> 2) & 3;
    if ty < 2 {
        let (n, hl) = match sf {
            0 | 2 => ((b0 >> 3) as usize, 1),
            1 => {
                let x = b.get(1).ok_or(Error::Corrupt("literals header"))?;
                ((b0 >> 4) as usize | (*x as usize) << 4, 2)
            }
            _ => {
                let x = b.get(1..3).ok_or(Error::Corrupt("literals header"))?;
                (
                    (b0 >> 4) as usize | (x[0] as usize) << 4 | (x[1] as usize) << 12,
                    3,
                )
            }
        };
        if n > MAX_BLOCK {
            return Err(Error::Corrupt("literals too large"));
        }
        if ty == 0 {
            let s = b
                .get(hl..hl + n)
                .ok_or(Error::Corrupt("raw literals truncated"))?;
            return Ok((s.to_vec(), hl + n));
        }
        b.get(hl).ok_or(Error::Corrupt("rle literal truncated"))?;
        return Ok((vec![b[hl]; n], hl + 1));
    }
    let (hl, bits, four) = match sf {
        0 => (3, 10, false),
        1 => (3, 10, true),
        2 => (4, 14, true),
        _ => (5, 18, true),
    };
    let h = b.get(..hl).ok_or(Error::Corrupt("literals header"))?;
    let mut v = 0u64;
    for (i, &x) in h.iter().enumerate() {
        v |= (x as u64) << (8 * i);
    }
    let regen = ((v >> 4) & ((1 << bits) - 1)) as usize;
    let comp = ((v >> (4 + bits)) & ((1 << bits) - 1)) as usize;
    if regen > MAX_BLOCK {
        return Err(Error::Corrupt("literals too large"));
    }
    let body = b
        .get(hl..hl + comp)
        .ok_or(Error::Corrupt("literals truncated"))?;
    let streams = if ty == 2 {
        let (t, used) = huff::read_tree(body)?;
        st.huff = Some(t);
        &body[used..]
    } else {
        body
    };
    let t = st
        .huff
        .as_ref()
        .ok_or(Error::Corrupt("missing huffman table"))?;
    Ok((huff::decode(t, streams, four, regen)?, hl + comp))
}

/// 某类序列码的表规格：（预定义分布，预定义精度，最大符号，精度上限）。
type Spec<'a> = (&'a [i16], u32, usize, u32);

fn seq_table(
    mode: u8,
    data: &[u8],
    pos: &mut usize,
    prev: &Option<FseTable>,
    spec: Spec,
) -> Result<FseTable> {
    let (default, default_al, max_sym, max_al) = spec;
    Ok(match mode {
        0 => fse::build(default, default_al)?,
        1 => {
            let s = *data.get(*pos).ok_or(Error::Corrupt("rle symbol"))?;
            if s as usize > max_sym {
                return Err(Error::Corrupt("rle symbol out of range"));
            }
            *pos += 1;
            fse::rle_table(s)
        }
        2 => {
            let (t, used) = fse::read_table(data.get(*pos..).unwrap_or(&[]), max_sym, max_al)?;
            *pos += used;
            t
        }
        _ => prev
            .clone()
            .ok_or(Error::Corrupt("repeat mode without table"))?,
    })
}

fn block(b: &[u8], st: &mut State, out: &mut Vec<u8>, start: usize, max_out: usize) -> Result<()> {
    if b.is_empty() {
        return Err(Error::Corrupt("empty block"));
    }
    let (lits, mut p) = literals(b, st)?;
    let n0 = *b.get(p).ok_or(Error::Corrupt("missing sequences header"))? as usize;
    p += 1;
    let nseq = if n0 == 0 {
        0
    } else if n0 < 128 {
        n0
    } else if n0 < 255 {
        let x = *b.get(p).ok_or(Error::Corrupt("sequence count"))? as usize;
        p += 1;
        ((n0 - 128) << 8) + x
    } else {
        let x = b.get(p..p + 2).ok_or(Error::Corrupt("sequence count"))?;
        p += 2;
        x[0] as usize + ((x[1] as usize) << 8) + 0x7F00
    };
    if nseq == 0 {
        return push(out, &lits, start, max_out);
    }
    let modes = *b.get(p).ok_or(Error::Corrupt("missing modes"))?;
    p += 1;
    if modes & 3 != 0 {
        return Err(Error::Corrupt("reserved modes bits"));
    }
    let ll = seq_table(modes >> 6, b, &mut p, &st.ll, (&LL_DEFAULT, 6, 35, 9))?;
    let of = seq_table((modes >> 4) & 3, b, &mut p, &st.of, (&OF_DEFAULT, 5, 31, 8))?;
    let ml = seq_table((modes >> 2) & 3, b, &mut p, &st.ml, (&ML_DEFAULT, 6, 52, 9))?;
    let mut r = RevBits::new(b.get(p..).unwrap_or(&[]))?;
    let (mut sl, mut so, mut sm) = (
        r.read(ll.al) as usize,
        r.read(of.al) as usize,
        r.read(ml.al) as usize,
    );
    let mut lp = 0usize;
    for i in 0..nseq {
        let (cl, co, cm) = (ll.cells[sl], of.cells[so], ml.cells[sm]);
        if cl.sym as usize >= LL_BASE.len() || cm.sym as usize >= ML_BASE.len() || co.sym > 31 {
            return Err(Error::Corrupt("sequence code out of range"));
        }
        let ofv = (1u32 << co.sym) + r.read(co.sym as u32);
        let (mb, mbits) = ML_BASE[cm.sym as usize];
        let mlen = (mb + r.read(mbits)) as usize;
        let (lb, lbits) = LL_BASE[cl.sym as usize];
        let llen = (lb + r.read(lbits)) as usize;
        if i + 1 < nseq {
            sl = cl.base as usize + r.read(cl.nb as u32) as usize;
            sm = cm.base as usize + r.read(cm.nb as u32) as usize;
            so = co.base as usize + r.read(co.nb as u32) as usize;
        }
        if r.overflow() {
            return Err(Error::Corrupt("sequence bitstream overrun"));
        }
        let off = if ofv > 3 {
            let o = ofv as usize - 3;
            st.hist = [o, st.hist[0], st.hist[1]];
            o
        } else {
            let k = if llen == 0 { ofv } else { ofv - 1 } as usize;
            let h = st.hist;
            match k {
                0 => h[0],
                1 => {
                    st.hist = [h[1], h[0], h[2]];
                    h[1]
                }
                2 => {
                    st.hist = [h[2], h[0], h[1]];
                    h[2]
                }
                _ => {
                    let o = h[0]
                        .checked_sub(1)
                        .filter(|&o| o > 0)
                        .ok_or(Error::Corrupt("zero offset"))?;
                    st.hist = [o, h[0], h[1]];
                    o
                }
            }
        };
        let lit = lits
            .get(lp..lp + llen)
            .ok_or(Error::Corrupt("literal run overrun"))?;
        lp += llen;
        if out.len() + llen + mlen > max_out {
            return Err(Error::TooLarge);
        }
        out.extend_from_slice(lit);
        if off == 0 || off > out.len() - start {
            return Err(Error::Corrupt("offset beyond history"));
        }
        let from = out.len() - off;
        // 重叠时已写出的部分以 off 为周期,分段复制
        let mut left = mlen;
        while left > 0 {
            let n = left.min(out.len() - from);
            out.extend_from_within(from..from + n);
            left -= n;
        }
    }
    if r.remaining() != 0 {
        return Err(Error::Corrupt("sequence bitstream not consumed"));
    }
    push(out, &lits[lp..], start, max_out)?;
    st.ll = Some(ll);
    st.of = Some(of);
    st.ml = Some(ml);
    Ok(())
}
