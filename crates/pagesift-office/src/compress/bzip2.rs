//! bzip2：哈夫曼 → 游程和 MTF → 反向 BWT → 第一层游程还原。支持串接的多个流。CRC 不验证。

use crate::Error;

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const MAX_GROUP: usize = 6;
const MAX_ALPHA: usize = 258;
const GROUP_SIZE: usize = 50;

fn bad(m: &str) -> Error {
    Error::Malformed(format!("bzip2: {m}"))
}

struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
}

impl Bits<'_> {
    fn take(&mut self, count: u32) -> Result<u64, Error> {
        while self.n < count {
            let v = *self.b.get(self.pos).ok_or_else(|| bad("truncated"))?;
            self.pos += 1;
            self.buf = self.buf << 8 | u64::from(v);
            self.n += 8;
        }
        self.n -= count;
        let out = (self.buf >> self.n) & ((1u64 << count) - 1);
        self.buf &= (1u64 << self.n) - 1;
        Ok(out)
    }

    fn align(&mut self) {
        let drop = self.n % 8;
        self.n -= drop;
        self.buf &= (1u64 << self.n) - 1;
    }
}

pub(crate) fn decode(input: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut bits = Bits {
        b: input,
        pos: 0,
        buf: 0,
        n: 0,
    };
    let mut out = Vec::new();
    loop {
        if bits.take(8)? != u64::from(b'B')
            || bits.take(8)? != u64::from(b'Z')
            || bits.take(8)? != u64::from(b'h')
        {
            return Err(bad("wrong signature"));
        }
        let level = bits.take(8)?;
        if !(u64::from(b'1')..=u64::from(b'9')).contains(&level) {
            return Err(bad("wrong block size"));
        }
        let block_max = (level - u64::from(b'0')) as usize * 100_000;
        loop {
            match bits.take(48)? {
                BLOCK_MAGIC => block(&mut bits, block_max, &mut out, limit)?,
                END_MAGIC => break,
                _ => return Err(bad("wrong block marker")),
            }
        }
        bits.take(32)?; // 整个流的 CRC
        bits.align();
        // 串接的下一个流；没有就结束
        if bits.n == 0 && bits.pos >= input.len() {
            return Ok(out);
        }
    }
}

fn block(bits: &mut Bits<'_>, max: usize, out: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    bits.take(32)?; // 块 CRC
    if bits.take(1)? != 0 {
        return Err(Error::Unsupported("randomised bzip2 block".into()));
    }
    let orig = bits.take(24)? as usize;
    // 用到的字节表：16 位总表，每位一组 16 位
    let used_groups = bits.take(16)?;
    let mut symbols: Vec<u8> = Vec::with_capacity(256);
    for i in 0..16u32 {
        if used_groups & (0x8000 >> i) != 0 {
            let set = bits.take(16)?;
            for j in 0..16u32 {
                if set & (0x8000 >> j) != 0 {
                    symbols.push((i * 16 + j) as u8);
                }
            }
        }
    }
    if symbols.is_empty() {
        return Err(bad("no symbols"));
    }
    let alpha = symbols.len() + 2;
    let groups = bits.take(3)? as usize;
    if !(2..=MAX_GROUP).contains(&groups) {
        return Err(bad("wrong table count"));
    }
    let selector_count = bits.take(15)? as usize;
    if selector_count == 0 {
        return Err(bad("no selectors"));
    }
    let mut order: Vec<u8> = (0..groups as u8).collect();
    let mut selectors = Vec::with_capacity(selector_count);
    for _ in 0..selector_count {
        let mut j = 0usize;
        while bits.take(1)? == 1 {
            j += 1;
            if j >= groups {
                return Err(bad("wrong selector"));
            }
        }
        let v = order.remove(j);
        order.insert(0, v);
        selectors.push(v);
    }
    let mut tables = Vec::with_capacity(groups);
    for _ in 0..groups {
        tables.push(Table::read(bits, alpha)?);
    }
    // 哈夫曼 → MTF + RUNA/RUNB
    let mut tt: Vec<u32> = Vec::with_capacity(max.min(1 << 20));
    let mut counts = [0u32; 256];
    let mut mtf: Vec<u8> = symbols;
    let end = alpha - 1;
    let (mut run, mut run_bit) = (0usize, 0u32);
    let (mut sel, mut left) = (0usize, 0usize);
    let mut table = &tables[0];
    loop {
        if left == 0 {
            let s = *selectors.get(sel).ok_or_else(|| bad("out of selectors"))?;
            table = &tables[usize::from(s)];
            sel += 1;
            left = GROUP_SIZE;
        }
        left -= 1;
        let sym = table.decode(bits)?;
        if sym <= 1 {
            if run_bit > 24 {
                return Err(bad("run too long"));
            }
            run += (sym + 1) << run_bit;
            run_bit += 1;
            continue;
        }
        if run > 0 {
            if tt.len() + run > max {
                return Err(bad("block too long"));
            }
            let v = mtf[0];
            counts[usize::from(v)] += run as u32;
            tt.resize(tt.len() + run, u32::from(v));
            run = 0;
            run_bit = 0;
        }
        if sym == end {
            break;
        }
        if tt.len() >= max {
            return Err(bad("block too long"));
        }
        let at = sym - 1;
        let v = *mtf.get(at).ok_or_else(|| bad("wrong symbol"))?;
        mtf.remove(at);
        mtf.insert(0, v);
        counts[usize::from(v)] += 1;
        tt.push(u32::from(v));
    }
    if tt.is_empty() {
        return Ok(());
    }
    if orig >= tt.len() {
        return Err(bad("wrong origin pointer"));
    }
    // 反向 BWT：高 24 位存下一项下标
    let mut starts = [0u32; 256];
    let mut sum = 0u32;
    for (s, c) in starts.iter_mut().zip(&counts) {
        *s = sum;
        sum += *c;
    }
    for i in 0..tt.len() {
        let b = (tt[i] & 0xFF) as usize;
        let at = starts[b] as usize;
        starts[b] += 1;
        tt[at] |= (i as u32) << 8;
    }
    // 第一层游程：连续 4 个相同字节后跟一个重复次数
    let mut pos = tt[orig] >> 8;
    let mut prev: Option<u8> = None;
    let mut same = 0u32;
    for _ in 0..tt.len() {
        let entry = tt[pos as usize];
        let b = (entry & 0xFF) as u8;
        pos = entry >> 8;
        if same == 4 {
            let n = usize::from(b);
            if out.len() + n > limit {
                return Err(Error::TooLarge("decompressed size"));
            }
            let last = prev.unwrap_or(0);
            out.resize(out.len() + n, last);
            same = 0;
            prev = None;
            continue;
        }
        same = if prev == Some(b) { same + 1 } else { 1 };
        prev = Some(b);
        if out.len() >= limit {
            return Err(Error::TooLarge("decompressed size"));
        }
        out.push(b);
    }
    Ok(())
}

/// 规范哈夫曼表。
struct Table {
    min_len: u32,
    limit: [i32; 22],
    base: [i32; 22],
    perm: Vec<u16>,
}

impl Table {
    fn read(bits: &mut Bits<'_>, alpha: usize) -> Result<Table, Error> {
        let mut lengths = [0u32; MAX_ALPHA];
        let mut cur = bits.take(5)? as i32;
        for l in lengths.iter_mut().take(alpha) {
            loop {
                if !(1..=20).contains(&cur) {
                    return Err(bad("wrong code length"));
                }
                if bits.take(1)? == 0 {
                    break;
                }
                cur += if bits.take(1)? == 0 { 1 } else { -1 };
            }
            *l = cur as u32;
        }
        let used = &lengths[..alpha];
        let min_len = used.iter().copied().min().unwrap_or(1);
        let max_len = used.iter().copied().max().unwrap_or(1);
        let mut perm = Vec::with_capacity(alpha);
        for len in min_len..=max_len {
            for (s, &l) in used.iter().enumerate() {
                if l == len {
                    perm.push(s as u16);
                }
            }
        }
        let mut count = [0i32; 22];
        for &l in used {
            count[l as usize] += 1;
        }
        let (mut limit, mut base) = ([i32::MAX; 22], [0i32; 22]);
        let (mut code, mut index) = (0i32, 0i32);
        for len in min_len..=max_len {
            let l = len as usize;
            base[l] = index - code;
            code += count[l];
            index += count[l];
            limit[l] = code - 1;
            code <<= 1;
        }
        Ok(Table {
            min_len,
            limit,
            base,
            perm,
        })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<usize, Error> {
        let mut len = self.min_len;
        let mut code = bits.take(len)? as i32;
        loop {
            if len > 20 {
                return Err(bad("wrong code"));
            }
            if code <= self.limit[len as usize] {
                let at = code + self.base[len as usize];
                return self
                    .perm
                    .get(at as usize)
                    .map(|&s| usize::from(s))
                    .ok_or_else(|| bad("wrong code"));
            }
            code = code << 1 | bits.take(1)? as i32;
            len += 1;
        }
    }
}
