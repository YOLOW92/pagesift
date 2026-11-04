//! 纯 Rust 的 zstd（RFC 8878）：完整解码器加基础编码器。无依赖、无 unsafe。
//!
//! - [`decompress`]：输入不可信，输出有上限，坏输入返回 [`Error`]，不会 panic。
//!   支持多帧、跳过帧、Raw/RLE/压缩块、FSE 与 Huffman（含 4 流、无树复用）、重复偏移。
//!   不支持字典（字典 ID 非 0 的帧报错）；有帧尾校验和(XXH64 低 32 位)就核对,不一致报错。
//! - [`compress`]：输出合法帧，贪心 LZ 匹配 + 预定义 FSE 表，字面量不做 Huffman（压缩率低于 C 版）。
#![forbid(unsafe_code)]

mod bits;
mod dec;
mod enc;
mod fse;
mod henc;
mod huff;
mod xxh;

pub use dec::{decompress, decompress_into};
pub use enc::compress;

/// 解码错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 数据格式不合法，附原因。
    Corrupt(&'static str),
    /// 输出超过调用方给的上限。
    TooLarge,
    /// 合法但本实现不支持（字典）。
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Corrupt(s) => write!(f, "zstd: corrupt data: {s}"),
            Error::TooLarge => write!(f, "zstd: output exceeds limit"),
            Error::Unsupported(s) => write!(f, "zstd: unsupported: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
