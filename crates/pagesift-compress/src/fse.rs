//! FSE 表：从表头解出分布、建解码表。
use crate::bits::FwdBits;
use crate::{Error, Result};

#[derive(Clone, Copy, Default)]
pub(crate) struct Cell {
    pub sym: u8,
    pub nb: u8,
    pub base: u16,
}

#[derive(Clone)]
pub(crate) struct FseTable {
    pub al: u32,
    pub cells: Vec<Cell>,
}

/// 读表头，返回（分布，精度位数，消耗字节）。`max_sym` 是允许的最大符号编号，`max_al` 是精度上限。
pub(crate) fn read_dist(
    data: &[u8],
    max_sym: usize,
    max_al: u32,
) -> Result<(Vec<i16>, u32, usize)> {
    let mut r = FwdBits::new(data);
    let al = r.read(4) + 5;
    if al > max_al {
        return Err(Error::Corrupt("fse accuracy too large"));
    }
    let mut remaining: i32 = (1 << al) + 1;
    let mut threshold: i32 = 1 << al;
    let mut nbits = al + 1;
    let mut probs: Vec<i16> = Vec::new();
    while remaining > 1 {
        if probs.len() > max_sym {
            return Err(Error::Corrupt("fse too many symbols"));
        }
        let max = 2 * threshold - 1 - remaining;
        let v = r.peek(nbits) as i32;
        let mut count = v & (threshold - 1);
        if count < max {
            r.skip(nbits - 1);
        } else {
            count = v & (2 * threshold - 1);
            if count >= threshold {
                count -= max;
            }
            r.skip(nbits);
        }
        count -= 1;
        remaining -= count.abs();
        if remaining < 1 {
            return Err(Error::Corrupt("fse distribution overflow"));
        }
        probs.push(count as i16);
        if count == 0 {
            loop {
                let rep = r.read(2) as usize;
                if probs.len() + rep > max_sym + 1 {
                    return Err(Error::Corrupt("fse zero run too long"));
                }
                probs.extend(std::iter::repeat_n(0, rep));
                if rep != 3 {
                    break;
                }
            }
        }
        while remaining < threshold {
            nbits -= 1;
            threshold >>= 1;
        }
    }
    if remaining != 1 {
        return Err(Error::Corrupt("fse distribution sum"));
    }
    let used = r.bytes_used()?;
    Ok((probs, al, used))
}

pub(crate) fn build(probs: &[i16], al: u32) -> Result<FseTable> {
    let size = 1usize << al;
    let mut cells = vec![Cell::default(); size];
    let mut high = size;
    let mut next = vec![0u32; probs.len()];
    for (s, &p) in probs.iter().enumerate() {
        if p == -1 {
            high -= 1;
            cells[high].sym = s as u8;
            next[s] = 1;
        } else {
            next[s] = p.max(0) as u32;
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mask = size - 1;
    let mut pos = 0usize;
    for (s, &p) in probs.iter().enumerate() {
        for _ in 0..p.max(0) {
            cells[pos].sym = s as u8;
            pos = (pos + step) & mask;
            while pos >= high {
                pos = (pos + step) & mask;
            }
        }
    }
    if pos != 0 {
        return Err(Error::Corrupt("fse table spread"));
    }
    for c in cells.iter_mut() {
        let x = next[c.sym as usize];
        next[c.sym as usize] += 1;
        let nb = al - (31 - x.leading_zeros());
        c.nb = nb as u8;
        c.base = ((x << nb) - size as u32) as u16;
    }
    Ok(FseTable { al, cells })
}

/// 读表头并建表。返回（表，消耗字节）。
pub(crate) fn read_table(data: &[u8], max_sym: usize, max_al: u32) -> Result<(FseTable, usize)> {
    let (p, al, used) = read_dist(data, max_sym, max_al)?;
    Ok((build(&p, al)?, used))
}

/// 单符号（RLE）表。
pub(crate) fn rle_table(sym: u8) -> FseTable {
    FseTable {
        al: 0,
        cells: vec![Cell {
            sym,
            nb: 0,
            base: 0,
        }],
    }
}
