//! 位读取器：正向（FSE 表头）和反向（块内数据，从末尾哨兵位往前读）。
use crate::{Error, Result};

/// 取 `data` 里从第 `start` 位起的 `n` 位（n ≤ 32），越界处补 0。位序：每字节低位在先。
fn get_bits(data: &[u8], start: usize, n: u32) -> u64 {
    let byte = start / 8;
    let mut v: u64 = 0;
    for i in 0..8 {
        if let Some(&b) = data.get(byte + i) {
            v |= (b as u64) << (8 * i);
        }
    }
    (v >> (start % 8)) & ((1u64 << n) - 1)
}

pub(crate) struct FwdBits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> FwdBits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn peek(&self, n: u32) -> u32 {
        get_bits(self.data, self.pos, n) as u32
    }
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }
    pub fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }
    /// 已消耗的字节数（向上取整）；读过头则报错。
    pub fn bytes_used(&self) -> Result<usize> {
        let b = self.pos.div_ceil(8);
        if b > self.data.len() {
            return Err(Error::Corrupt("fse header truncated"));
        }
        Ok(b)
    }
}

/// 反向位读取器:从末尾哨兵位往前读,64 位缓冲高位对齐,读空后补 0 并记为越界。
pub(crate) struct RevBits<'a> {
    data: &'a [u8],
    /// 还没装进缓冲的字节数(data[..idx])。
    idx: usize,
    buf: u64,
    have: u32,
    /// 剩余有效位数;读过头会变负。
    rem: i64,
}

impl<'a> RevBits<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let last = *data.last().ok_or(Error::Corrupt("empty bitstream"))?;
        if last == 0 {
            return Err(Error::Corrupt("bitstream without end mark"));
        }
        let lz = last.leading_zeros();
        let have = 7 - lz;
        let idx = data.len() - 1;
        Ok(Self {
            data,
            idx,
            buf: ((last as u64) << 56) << (lz + 1),
            have,
            rem: have as i64 + 8 * idx as i64,
        })
    }
    #[inline(always)]
    fn refill(&mut self) {
        if self.have < 32 {
            if self.idx >= 4 {
                let b = &self.data[self.idx - 4..self.idx];
                let w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                self.buf |= (w as u64) << (32 - self.have);
                self.have += 32;
                self.idx -= 4;
            } else {
                while self.have <= 56 && self.idx > 0 {
                    self.idx -= 1;
                    self.buf |= (self.data[self.idx] as u64) << (56 - self.have);
                    self.have += 8;
                }
            }
        }
    }
    /// 读 `n` 位(n ≤ 32);越过开头的部分补 0 并让 `overflow()` 为真。
    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }
    #[inline(always)]
    pub fn peek(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.refill();
        (self.buf >> (64 - n)) as u32
    }
    #[inline(always)]
    pub fn skip(&mut self, n: u32) {
        self.buf <<= n;
        self.have = self.have.saturating_sub(n);
        self.rem -= n as i64;
    }
    pub fn overflow(&self) -> bool {
        self.rem < 0
    }
    pub fn remaining(&self) -> i64 {
        self.rem
    }
}
