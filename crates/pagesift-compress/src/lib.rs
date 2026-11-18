#![forbid(unsafe_code)]

//! Pure-Rust Zstandard (RFC 8878) codec facade over [`yolowzstd`] with exact uncompressable/raw
//! preallocation and bounded thread-local working-set retention.

use std::cell::RefCell;

pub use yolowzstd::{
    CompressOptions, Compressor, DecompressOptions, Decompressor, Error, compress_bound,
};

/// [策略] 单次编解码输入或输出超过此阈值（4 MiB）时，回收线程局部编解码器工作集，避免常驻线程长期占留大哈希表或字面量缓冲。
const MAX_RETAINED_TLS_BYTES: usize = 4 << 20;
/// [策略] 压缩输出尾部空闲容量超过 64 KiB 且比例超 25% 时收缩，避免不可压缩输入触发 `Vec` 倍增后长期驻留双倍内存。
const MIN_EXCESS_SHRINK_BYTES: usize = 64 << 10;

/// [策略] 小帧（<= 128B）头部占比高，直接按 RFC 8878 最坏帧上界一次分配；更大输入先按一半加 64B 冗余预分配。
const SMALL_FRAME_BOUND_THRESHOLD: usize = 128;
const FRAME_HEADER_SLACK_BYTES: usize = 64; // [策略] Zstd 常规帧头与块头预留冗余字节

thread_local! {
    static ENC_TLS: RefCell<Option<Box<Compressor>>> = const { RefCell::new(None) };
    static DEC_TLS: RefCell<Option<Box<Decompressor>>> = const { RefCell::new(None) };
}

#[inline]
fn initial_compress_capacity(len: usize, level: i32) -> usize {
    if level <= 0 || len <= SMALL_FRAME_BOUND_THRESHOLD {
        compress_bound(len)
    } else {
        len / 2 + FRAME_HEADER_SLACK_BYTES
    }
}

#[inline]
fn trim_excess_capacity(out: &mut Vec<u8>) {
    let len = out.len();
    let cap = out.capacity();
    if cap.saturating_sub(len) > MIN_EXCESS_SHRINK_BYTES && cap > len.saturating_add(len >> 2) {
        out.shrink_to_fit();
    }
}

/// 压缩成一个 Zstd 帧。`level <= 0` 精确一次预分配 Raw/RLE 上界，避免倍增重分配；大输入后自动回收 TLS 工作集。
pub fn compress(data: &[u8], level: i32) -> Vec<u8> {
    compress_with(data, &CompressOptions::new(level))
}

/// 按自定义 [`CompressOptions`] 压缩 `data` 并返回紧凑帧缓冲。
pub fn compress_with(data: &[u8], opts: &CompressOptions) -> Vec<u8> {
    let mut out = Vec::with_capacity(initial_compress_capacity(data.len(), opts.level));
    compress_into(data, &mut out, opts);
    trim_excess_capacity(&mut out);
    out
}

/// 按 [`CompressOptions`] 将 `data` 压缩写入调用方缓冲 `out`。
pub fn compress_into(data: &[u8], out: &mut Vec<u8>, opts: &CompressOptions) {
    if opts.level <= 0 {
        out.reserve(compress_bound(data.len()));
    }
    ENC_TLS.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            let cmp = guard.get_or_insert_with(|| Box::new(Compressor::new()));
            cmp.compress_into(data, out, opts);
            if data.len() > MAX_RETAINED_TLS_BYTES {
                cmp.shrink_to_fit();
            }
        } else {
            let mut cmp = Compressor::new();
            cmp.compress_into(data, out, opts);
        }
    });
}

/// 解压 `data`（支持多帧与跳过帧），输出不超过 `max_out` 字节。
pub fn decompress(data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    decompress_into(data, &mut out, max_out)?;
    trim_excess_capacity(&mut out);
    Ok(out)
}

/// 按 [`DecompressOptions`] 解压 `data`（多帧输入支持多线程并行）。
pub fn decompress_with(
    data: &[u8],
    opts: &DecompressOptions,
    max_out: usize,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    decompress_into_with(data, &mut out, opts, max_out)?;
    trim_excess_capacity(&mut out);
    Ok(out)
}

/// 解压 `data` 并追加写入调用方缓冲 `out`。
pub fn decompress_into(data: &[u8], out: &mut Vec<u8>, max_out: usize) -> Result<(), Error> {
    decompress_into_with(data, out, &DecompressOptions::default(), max_out)
}

/// 按 [`DecompressOptions`] 解压 `data` 并追加写入调用方缓冲 `out`；超大输入/输出后自动回收 TLS 缓冲。
pub fn decompress_into_with(
    data: &[u8],
    out: &mut Vec<u8>,
    opts: &DecompressOptions,
    max_out: usize,
) -> Result<(), Error> {
    let before = out.len();
    DEC_TLS.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            let dec = guard.get_or_insert_with(|| Box::new(Decompressor::new()));
            let res = dec.decompress_into_with(data, out, opts, max_out);
            let produced = out.len().saturating_sub(before);
            if data.len() > MAX_RETAINED_TLS_BYTES || produced > MAX_RETAINED_TLS_BYTES {
                dec.shrink_to_fit();
            }
            res
        } else {
            let mut dec = Decompressor::new();
            dec.decompress_into_with(data, out, opts, max_out)
        }
    })
}
