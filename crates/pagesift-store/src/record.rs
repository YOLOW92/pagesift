//! 记录格式与段文件扫描。
//!
//! 段文件 = 8 字节魔数 + 若干记录。每条记录：
//! `[body_len u32][crc32(body) u32][ns u8][op u8][ts_ms u64][key_len u16][key][value]`，
//! 整数都是小端。

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

pub(crate) const SEG_MAGIC: &[u8; 8] = b"PSSTORE1";
pub(crate) const SEG_HEADER: u64 = SEG_MAGIC.len() as u64;
pub(crate) const REC_HEADER: usize = 8;
const BODY_FIXED: usize = 12;
/// 单条记录（含头）的上限，索引里长度只占 28 位。
pub(crate) const MAX_RECORD: usize = (1 << 28) - 1;
pub(crate) const MAX_KEY: usize = u16::MAX as usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Put = 1,
    Del = 2,
}

pub(crate) struct RecordRef<'a> {
    pub ns: u8,
    pub op: Op,
    pub ts_ms: u64,
    pub key: &'a [u8],
    pub value: &'a [u8],
}

pub(crate) fn encoded_len(key: &[u8], value: &[u8]) -> usize {
    REC_HEADER + BODY_FIXED + key.len() + value.len()
}

pub(crate) fn encode(ns: u8, op: Op, ts_ms: u64, key: &[u8], value: &[u8]) -> Vec<u8> {
    let body_len = BODY_FIXED + key.len() + value.len();
    let mut buf = Vec::with_capacity(REC_HEADER + body_len);
    buf.extend_from_slice(&(body_len as u32).to_le_bytes());
    buf.extend_from_slice(&[0; 4]);
    buf.push(ns);
    buf.push(op as u8);
    buf.extend_from_slice(&ts_ms.to_le_bytes());
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(value);
    let crc = crc32fast::hash(&buf[REC_HEADER..]);
    buf[4..8].copy_from_slice(&crc.to_le_bytes());
    buf
}

/// 解析完整记录（含头），长度或校验和不对时返回 None。
pub(crate) fn decode(raw: &[u8]) -> Option<RecordRef<'_>> {
    if raw.len() < REC_HEADER + BODY_FIXED {
        return None;
    }
    let body_len = u32::from_le_bytes(raw[0..4].try_into().ok()?) as usize;
    if body_len != raw.len() - REC_HEADER {
        return None;
    }
    let crc = u32::from_le_bytes(raw[4..8].try_into().ok()?);
    let body = &raw[REC_HEADER..];
    if crc32fast::hash(body) != crc {
        return None;
    }
    decode_body(body)
}

fn decode_body(body: &[u8]) -> Option<RecordRef<'_>> {
    let op = match body[1] {
        1 => Op::Put,
        2 => Op::Del,
        _ => return None,
    };
    let ts_ms = u64::from_le_bytes(body[2..10].try_into().ok()?);
    let key_len = u16::from_le_bytes(body[10..12].try_into().ok()?) as usize;
    let key = body.get(BODY_FIXED..BODY_FIXED + key_len)?;
    Some(RecordRef {
        ns: body[0],
        op,
        ts_ms,
        key,
        value: &body[BODY_FIXED + key_len..],
    })
}

/// 只比对命名空间和键，不校验 CRC。`prefix` 至少要有 `key_probe_len(key)` 字节。
pub(crate) fn key_probe_len(key: &[u8]) -> usize {
    REC_HEADER + BODY_FIXED + key.len()
}

pub(crate) fn key_matches(prefix: &[u8], ns: u8, key: &[u8]) -> bool {
    prefix.len() >= key_probe_len(key)
        && prefix[REC_HEADER] == ns
        && prefix[REC_HEADER + 10..REC_HEADER + 12] == (key.len() as u16).to_le_bytes()
        && &prefix[REC_HEADER + BODY_FIXED..REC_HEADER + BODY_FIXED + key.len()] == key
}

/// 顺序扫描段文件里的记录，返回最后一条完整记录之后的偏移。
/// 遇到写了一半或校验失败的记录就停下，后面的内容视为无效。
pub(crate) fn scan<E: From<io::Error>>(
    file: &File,
    file_len: u64,
    mut f: impl FnMut(u32, &[u8], RecordRef<'_>) -> Result<(), E>,
) -> Result<u64, E> {
    let mut reader = BufReader::with_capacity(1 << 20, file);
    reader.seek(SeekFrom::Start(SEG_HEADER))?;
    let mut off = SEG_HEADER;
    let mut buf = Vec::new();
    loop {
        if off + REC_HEADER as u64 > file_len {
            return Ok(off);
        }
        buf.resize(REC_HEADER, 0);
        if read_full(&mut reader, &mut buf)? < REC_HEADER {
            return Ok(off);
        }
        let body_len = u32::from_le_bytes(buf[0..4].try_into().expect("4 bytes")) as usize;
        let total = REC_HEADER + body_len;
        if body_len < BODY_FIXED || total > MAX_RECORD || off + total as u64 > file_len {
            return Ok(off);
        }
        buf.resize(total, 0);
        if read_full(&mut reader, &mut buf[REC_HEADER..])? < body_len {
            return Ok(off);
        }
        let Some(rec) = decode(&buf) else {
            return Ok(off);
        };
        f(off as u32, &buf, rec)?;
        off += total as u64;
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(unix)]
pub(crate) fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}

#[cfg(unix)]
pub(crate) fn write_all_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, off) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                off += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_write(buf, off) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                buf = &buf[n..];
                off += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
