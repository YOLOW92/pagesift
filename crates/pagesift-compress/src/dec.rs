//! 帧、块、字面量段、序列段。
use crate::bits::RevBits;
use crate::fse::{self, FseTable};
use crate::huff::{self, HuffTable};
use crate::{Error, Result};

const MAX_BLOCK: usize = 128 * 1024;

pub(crate) const LL_BASE: [(u32, u32); 36] = [
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
pub(crate) const ML_BASE: [(u32, u32); 53] = [
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
    ll: Option<SeqTable>,
    of: Option<SeqTable>,
    ml: Option<SeqTable>,
    /// 序列暂存（ll, ml, off），跨块复用。
    seqs: Vec<(u32, u32, u32)>,
}

/// 序列码展开后的解码格：状态转移（base、nb）加上额外位数和基值，省掉每条序列的查表。
#[derive(Clone, Copy, Default)]
struct SCell {
    base: u16,
    nb: u8,
    eb: u8,
    val: u32,
}

#[derive(Clone)]
struct SeqTable {
    al: u32,
    /// 定长 512 格（精度上限 9 位），用掩码下标，热循环里没有边界检查。
    cells: Box<[SCell; 512]>,
}

#[derive(Clone, Copy)]
enum Kind {
    Ll,
    Of,
    Ml,
}

fn expand(t: FseTable, kind: Kind) -> SeqTable {
    let mut cells = Box::new([SCell::default(); 512]);
    for (dst, c) in cells.iter_mut().zip(t.cells.iter()) {
        let sym = c.sym as usize;
        let (val, eb) = match kind {
            Kind::Ll => LL_BASE[sym.min(LL_BASE.len() - 1)],
            Kind::Ml => ML_BASE[sym.min(ML_BASE.len() - 1)],
            Kind::Of => (1u32 << sym.min(31), sym.min(31) as u32),
        };
        *dst = SCell {
            base: c.base,
            nb: c.nb,
            eb: eb as u8,
            val,
        };
    }
    SeqTable { al: t.al, cells }
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
        seqs: Vec::new(),
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
    prev: &Option<SeqTable>,
    spec: Spec,
    kind: Kind,
) -> Result<SeqTable> {
    let (default, default_al, max_sym, max_al) = spec;
    Ok(match mode {
        0 => expand(fse::build(default, default_al)?, kind),
        1 => {
            let s = *data.get(*pos).ok_or(Error::Corrupt("rle symbol"))?;
            if s as usize > max_sym {
                return Err(Error::Corrupt("rle symbol out of range"));
            }
            *pos += 1;
            expand(fse::rle_table(s), kind)
        }
        2 => {
            let (t, used) = fse::read_table(data.get(*pos..).unwrap_or(&[]), max_sym, max_al)?;
            *pos += used;
            expand(t, kind)
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
    let (mut lits, mut p) = literals(b, st)?;
    let lit_len = lits.len();
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
    let ll = seq_table(
        modes >> 6,
        b,
        &mut p,
        &st.ll,
        (&LL_DEFAULT, 6, 35, 9),
        Kind::Ll,
    )?;
    let of = seq_table(
        (modes >> 4) & 3,
        b,
        &mut p,
        &st.of,
        (&OF_DEFAULT, 5, 31, 8),
        Kind::Of,
    )?;
    let ml = seq_table(
        (modes >> 2) & 3,
        b,
        &mut p,
        &st.ml,
        (&ML_DEFAULT, 6, 52, 9),
        Kind::Ml,
    )?;
    let mut r = RevBits::new(b.get(p..).unwrap_or(&[]))?;
    let (mut sl, mut so, mut sm) = (
        r.read(ll.al) as usize,
        r.read(of.al) as usize,
        r.read(ml.al) as usize,
    );
    // 第一遍：只解序列（位流工作），攒进暂存区并累计总长
    let seqs = &mut st.seqs;
    seqs.clear();
    let mut hist = st.hist;
    let (mut sum_ll, mut sum_ml) = (0u64, 0u64);
    seqs.resize(nseq, (0, 0, 0));
    for (i, slot) in seqs.iter_mut().enumerate() {
        let (cl, co, cm) = (ll.cells[sl & 511], of.cells[so & 511], ml.cells[sm & 511]);
        let (eo, em, el) = (co.eb as u32, cm.eb as u32, cl.eb as u32);
        let (nl, nm, no) = (cl.nb as u32, cm.nb as u32, co.nb as u32);
        let te = eo + em + el;
        let tn = if i + 1 < nseq { nl + nm + no } else { 0 };
        // 一次补缓冲、一次读出本条序列的全部位（额外位 of/ml/ll 在高位，状态更新 ll/ml/of 在低位），
        // 再用移位掩码拆开：位缓冲上只有一条串行依赖
        r.refill_wide();
        let m = |n: u32| (1u64 << n) - 1;
        let (xo, xm, xl, ul, um, uo);
        if te + tn <= 56 {
            let v = r.read_wide(te + tn);
            uo = v & m(no);
            um = (v >> no) & m(nm);
            ul = (v >> (no + nm)) & m(nl);
            xl = (v >> tn) & m(el);
            xm = (v >> (tn + el)) & m(em);
            xo = v >> (tn + el + em);
        } else {
            xo = r.read(eo) as u64;
            xm = r.read(em) as u64;
            xl = r.read(el) as u64;
            if tn > 0 {
                ul = r.read(nl) as u64;
                um = r.read(nm) as u64;
                uo = r.read(no) as u64;
            } else {
                (ul, um, uo) = (0, 0, 0);
            }
        }
        sl = cl.base as usize + ul as usize;
        sm = cm.base as usize + um as usize;
        so = co.base as usize + uo as usize;
        let ofv = co.val + xo as u32;
        let mlen = cm.val + xm as u32;
        let llen = cl.val + xl as u32;
        let off = if ofv > 3 {
            let o = (ofv - 3) as usize;
            hist = [o, hist[0], hist[1]];
            o
        } else {
            let k = if llen == 0 { ofv } else { ofv - 1 };
            match k {
                0 => hist[0],
                1 => {
                    hist = [hist[1], hist[0], hist[2]];
                    hist[0]
                }
                2 => {
                    hist = [hist[2], hist[0], hist[1]];
                    hist[0]
                }
                _ => {
                    let o = hist[0]
                        .checked_sub(1)
                        .filter(|&o| o > 0)
                        .ok_or(Error::Corrupt("zero offset"))?;
                    hist = [o, hist[0], hist[1]];
                    o
                }
            }
        };
        sum_ll += llen as u64;
        sum_ml += mlen as u64;
        *slot = (llen, mlen, off as u32);
    }
    if r.overflow() {
        return Err(Error::Corrupt("sequence bitstream overrun"));
    }
    if r.remaining() != 0 {
        return Err(Error::Corrupt("sequence bitstream not consumed"));
    }
    if sum_ll > lit_len as u64 {
        return Err(Error::Corrupt("literal run overrun"));
    }
    let total = sum_ml + lit_len as u64;
    if total > (max_out.saturating_sub(out.len())) as u64 {
        return Err(Error::TooLarge);
    }
    st.hist = hist;
    // 第二遍：复制。输出区先放大（带 32 字节余量），按 16 字节定长块复制，超出部分稍后被覆盖或截掉。
    lits.resize(lit_len + 16, 0);
    let base = out.len();
    let end = base + total as usize;
    out.resize(end + 32, 0);
    let (mut pos, mut lp) = (base, 0usize);
    for &(llen, mlen, off) in st.seqs.iter() {
        let (llen, mlen, off) = (llen as usize, mlen as usize, off as usize);
        let mut left = llen;
        while left > 0 {
            out[pos..pos + 16].copy_from_slice(&lits[lp..lp + 16]);
            let n = left.min(16);
            pos += n;
            lp += n;
            left -= n;
        }
        if off > pos - start {
            out.truncate(base);
            return Err(Error::Corrupt("offset beyond history"));
        }
        let from = pos - off;
        if off >= 16 {
            let mut done = 0;
            while done < mlen {
                out.copy_within(from + done..from + done + 16, pos + done);
                done += 16;
            }
        } else {
            // 重叠：已写出的部分以 off 为周期，分段复制
            let mut done = 0;
            while done < mlen {
                let n = (mlen - done).min(pos + done - from);
                out.copy_within(from..from + n, pos + done);
                done += n;
            }
        }
        pos += mlen;
    }
    let rest = lit_len - lp;
    out[pos..pos + rest].copy_from_slice(&lits[lp..lp + rest]);
    out.truncate(end);
    st.ll = Some(ll);
    st.of = Some(of);
    st.ml = Some(ml);
    Ok(())
}
