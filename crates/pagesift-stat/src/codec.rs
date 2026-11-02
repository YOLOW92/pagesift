//! 统计量的二进制编码：小端定长整数和浮点，长度前缀的序列。
//!
//! 格式版本由上层（[`crate::table`]）统一写在表头；单个统计量不带版本，版本不兼容时整表丢弃。

use std::fmt;

/// 解码失败：数据被截断或内容不合法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    Invalid(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("stat encoding truncated"),
            Self::Invalid(what) => write!(f, "invalid stat encoding: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// 追加写入的缓冲。
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// 长度前缀（u32）的浮点序列。
    pub fn f64s(&mut self, v: &[f64]) {
        self.len(v.len());
        for &x in v {
            self.f64(x);
        }
    }

    /// 长度前缀（u32）的整数序列。
    pub fn u64s(&mut self, v: &[u64]) {
        self.len(v.len());
        for &x in v {
            self.u64(x);
        }
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.len(v.len());
        self.buf.extend_from_slice(v);
    }

    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("stat sequence longer than u32::MAX"));
    }
}

/// 顺序读取。
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        if self.buf.len() < N {
            return Err(DecodeError::Truncated);
        }
        let (head, rest) = self.buf.split_at(N);
        self.buf = rest;
        Ok(head.try_into().expect("split_at gives N bytes"))
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take::<1>()?[0])
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take()?))
    }

    pub fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.take()?))
    }

    pub fn f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_le_bytes(self.take()?))
    }

    /// 序列长度；按每个元素至少 `elem` 字节检查剩余长度，防止恶意长度触发大分配。
    fn len(&mut self, elem: usize) -> Result<usize, DecodeError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(elem) > self.buf.len() {
            return Err(DecodeError::Truncated);
        }
        Ok(n)
    }

    pub fn f64s(&mut self) -> Result<Vec<f64>, DecodeError> {
        let n = self.len(size_of::<f64>())?;
        (0..n).map(|_| self.f64()).collect()
    }

    pub fn u64s(&mut self) -> Result<Vec<u64>, DecodeError> {
        let n = self.len(size_of::<u64>())?;
        (0..n).map(|_| self.u64()).collect()
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.len(1)?;
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_truncation() {
        let mut w = Writer::new();
        w.u8(7);
        w.u64(u64::MAX);
        w.f64(-2.5);
        w.f64s(&[1.0, 2.0]);
        w.bytes(b"ab");
        let buf = w.into_inner();
        let mut r = Reader::new(&buf);
        assert_eq!(r.u8(), Ok(7));
        assert_eq!(r.u64(), Ok(u64::MAX));
        assert_eq!(r.f64(), Ok(-2.5));
        assert_eq!(r.f64s(), Ok(vec![1.0, 2.0]));
        assert_eq!(r.bytes(), Ok(&b"ab"[..]));
        assert!(r.is_empty());
        let mut short = Reader::new(&buf[..buf.len() - 1]);
        short.u8().unwrap();
        short.u64().unwrap();
        short.f64().unwrap();
        short.f64s().unwrap();
        assert_eq!(short.bytes(), Err(DecodeError::Truncated));
        // 声称很长的序列不会触发大分配
        let mut w = Writer::new();
        w.u32(u32::MAX);
        assert_eq!(
            Reader::new(w.as_slice()).f64s(),
            Err(DecodeError::Truncated)
        );
    }
}
