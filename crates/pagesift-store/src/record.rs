//! 记录格式与段文件扫描。
//!
//! 段文件 = 8 字节魔数 + 若干记录。每条记录：
//! `[body_len u32][crc32(body) u32][ns u8][op u8][ts_ms u64][key_len u16][key][value]`，
//! 整数都是小端。
//!
//! `AtomicTx` 使用 `TxPut` / `TxDel` 写出操作序列并在末尾追加一条 `TxCommit` 记录
//! （`key = tx_id: u64`, `value = [count: u32][blake3: 32B]`），确保任意位置崩溃均可全有或全无恢复。

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

pub(crate) const SEG_MAGIC: &[u8; 8] = b"PSSTORE1";
pub(crate) const SEG_HEADER: u64 = SEG_MAGIC.len() as u64;
pub(crate) const REC_HEADER: usize = 8;
const BODY_FIXED: usize = 12;
/// 单条记录（含头）的上限，索引里长度只占 28 位。
pub(crate) const MAX_RECORD: usize = (1 << 28) - 1;
pub(crate) const MAX_KEY: usize = u16::MAX as usize;
pub(crate) const TX_HASH_DOMAIN: &[u8] = b"PSSTORE1-TX-V1";
pub(crate) const TX_COMMIT_VALUE_LEN: usize = 4 + 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Put = 1,
    Del = 2,
    TxPut = 3,
    TxDel = 4,
    TxCommit = 5,
}

impl Op {
    pub(crate) fn is_put(self) -> bool {
        matches!(self, Self::Put | Self::TxPut)
    }

    pub(crate) fn is_del(self) -> bool {
        matches!(self, Self::Del | Self::TxDel)
    }
}

pub(crate) struct RecordRef<'a> {
    pub ns: u8,
    pub op: Op,
    pub ts_ms: u64,
    pub key: &'a [u8],
    pub value: &'a [u8],
}

pub(crate) struct ScannedRecord<'a> {
    pub off: u32,
    pub len: u32,
    pub ns: u8,
    pub op: Op,
    pub ts_ms: u64,
    pub key: &'a [u8],
}

pub(crate) fn encoded_len(key: &[u8], value: &[u8]) -> usize {
    REC_HEADER + BODY_FIXED + key.len() + value.len()
}

pub(crate) fn tx_commit_encoded_len() -> usize {
    REC_HEADER + BODY_FIXED + 8 + TX_COMMIT_VALUE_LEN
}

pub(crate) fn encode(ns: u8, op: Op, ts_ms: u64, key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(encoded_len(key, value));
    encode_into(&mut buf, ns, op, ts_ms, key, value);
    buf
}

pub(crate) fn encode_into(buf: &mut Vec<u8>, ns: u8, op: Op, ts_ms: u64, key: &[u8], value: &[u8]) {
    let start = buf.len();
    let body_len = BODY_FIXED + key.len() + value.len();
    buf.reserve(REC_HEADER + body_len);
    buf.extend_from_slice(&(body_len as u32).to_le_bytes());
    buf.extend_from_slice(&[0; 4]);
    buf.push(ns);
    buf.push(op as u8);
    buf.extend_from_slice(&ts_ms.to_le_bytes());
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(value);
    let crc = crc32fast::hash(&buf[start + REC_HEADER..]);
    buf[start + 4..start + 8].copy_from_slice(&crc.to_le_bytes());
}

pub(crate) fn tx_commit_digest(
    tx_id: u64,
    op_count: u32,
    mut raw_ops_hasher: blake3::Hasher,
) -> [u8; 32] {
    raw_ops_hasher.update(&op_count.to_le_bytes());
    raw_ops_hasher.update(&tx_id.to_le_bytes());
    *raw_ops_hasher.finalize().as_bytes()
}

pub(crate) fn encode_tx_commit(
    buf: &mut Vec<u8>,
    tx_id: u64,
    ts_ms: u64,
    op_count: u32,
    raw_ops_hasher: blake3::Hasher,
) {
    let digest = tx_commit_digest(tx_id, op_count, raw_ops_hasher);
    let mut val = [0u8; TX_COMMIT_VALUE_LEN];
    val[0..4].copy_from_slice(&op_count.to_le_bytes());
    val[4..36].copy_from_slice(&digest);
    encode_into(buf, 0, Op::TxCommit, ts_ms, &tx_id.to_le_bytes(), &val);
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
        3 => Op::TxPut,
        4 => Op::TxDel,
        5 => Op::TxCommit,
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

struct PendingTxOp {
    off: u32,
    len: u32,
    ns: u8,
    op: Op,
    ts_ms: u64,
    key: Vec<u8>,
}

/// 顺序扫描段文件里的已提交记录，返回最后一条完整提交边界之后的偏移。
/// 遇到未提交事务（缺少匹配的 `TxCommit`）、写了一半或校验失败的记录就停下并回退到最后提交边界。
pub(crate) fn scan<E: From<io::Error>>(
    file: &File,
    file_len: u64,
    mut f: impl FnMut(ScannedRecord<'_>) -> Result<(), E>,
) -> Result<u64, E> {
    let mut reader = BufReader::with_capacity(1 << 20, file);
    reader.seek(SeekFrom::Start(SEG_HEADER))?;
    let mut off = SEG_HEADER;
    let mut committed_end = SEG_HEADER;
    let mut buf = Vec::new();
    let mut pending: Vec<PendingTxOp> = Vec::new();
    let mut tx_hasher = blake3::Hasher::new();

    loop {
        if off + REC_HEADER as u64 > file_len {
            return Ok(committed_end);
        }
        buf.resize(REC_HEADER, 0);
        if read_full(&mut reader, &mut buf)? < REC_HEADER {
            return Ok(committed_end);
        }
        let body_len = u32::from_le_bytes(buf[0..4].try_into().expect("4 bytes")) as usize;
        let total = REC_HEADER + body_len;
        if body_len < BODY_FIXED || total > MAX_RECORD || off + total as u64 > file_len {
            return Ok(committed_end);
        }
        buf.resize(total, 0);
        if read_full(&mut reader, &mut buf[REC_HEADER..])? < body_len {
            return Ok(committed_end);
        }
        let Some(rec) = decode(&buf) else {
            return Ok(committed_end);
        };
        let Ok(rec_off) = u32::try_from(off) else {
            return Ok(committed_end);
        };
        let rec_len = total as u32;

        match rec.op {
            Op::Put | Op::Del => {
                if !pending.is_empty() {
                    return Ok(committed_end);
                }
                f(ScannedRecord {
                    off: rec_off,
                    len: rec_len,
                    ns: rec.ns,
                    op: rec.op,
                    ts_ms: rec.ts_ms,
                    key: rec.key,
                })?;
                off += total as u64;
                committed_end = off;
            }
            Op::TxPut | Op::TxDel => {
                if pending.is_empty() {
                    tx_hasher = blake3::Hasher::new();
                    tx_hasher.update(TX_HASH_DOMAIN);
                }
                if u32::try_from(pending.len() + 1).is_err() {
                    return Ok(committed_end);
                }
                tx_hasher.update(&buf);
                pending.push(PendingTxOp {
                    off: rec_off,
                    len: rec_len,
                    ns: rec.ns,
                    op: rec.op,
                    ts_ms: rec.ts_ms,
                    key: rec.key.to_vec(),
                });
                off += total as u64;
            }
            Op::TxCommit => {
                if rec.ns != 0
                    || pending.is_empty()
                    || rec.key.len() != 8
                    || rec.value.len() != TX_COMMIT_VALUE_LEN
                {
                    return Ok(committed_end);
                }
                let tx_id = u64::from_le_bytes(rec.key[0..8].try_into().expect("8 bytes"));
                let count = u32::from_le_bytes(rec.value[0..4].try_into().expect("4 bytes"));
                if pending.len() != count as usize {
                    return Ok(committed_end);
                }
                let expected = tx_commit_digest(
                    tx_id,
                    count,
                    std::mem::replace(&mut tx_hasher, blake3::Hasher::new()),
                );
                if rec.value[4..36] != expected {
                    return Ok(committed_end);
                }
                for p in pending.drain(..) {
                    f(ScannedRecord {
                        off: p.off,
                        len: p.len,
                        ns: p.ns,
                        op: p.op,
                        ts_ms: p.ts_ms,
                        key: &p.key,
                    })?;
                }
                f(ScannedRecord {
                    off: rec_off,
                    len: rec_len,
                    ns: rec.ns,
                    op: Op::TxCommit,
                    ts_ms: tx_id,
                    key: &[],
                })?;
                off += total as u64;
                committed_end = off;
            }
        }
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
