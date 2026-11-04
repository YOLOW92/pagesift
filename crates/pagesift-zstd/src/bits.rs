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

pub(crate) struct RevBits<'a> {
    data: &'a [u8],
    /// 还剩多少位；读过头会变负。
    pos: i64,
}

impl<'a> RevBits<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let last = *data.last().ok_or(Error::Corrupt("empty bitstream"))?;
        if last == 0 {
            return Err(Error::Corrupt("bitstream without end mark"));
        }
        let total = data.len() as i64 * 8 - (last.leading_zeros() as i64 + 1);
        Ok(Self { data, pos: total })
    }
    /// 读 `n` 位（n ≤ 32）；越过开头的部分补 0 并让 `overflow()` 为真。
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.pos -= n as i64;
        v
    }
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let start = self.pos - n as i64;
        if start >= 0 {
            get_bits(self.data, start as usize, n) as u32
        } else if self.pos > 0 {
            (get_bits(self.data, 0, self.pos as u32) << (-start)) as u32
        } else {
            0
        }
    }
    pub fn skip(&mut self, n: u32) {
        self.pos -= n as i64;
    }
    pub fn overflow(&self) -> bool {
        self.pos < 0
    }
    pub fn remaining(&self) -> i64 {
        self.pos
    }
}
