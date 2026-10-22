//! 纯 Rust 多字节编码识别：BOM 与无 BOM 探测（UTF-8, UTF-16LE, UTF-16BE, UTF-32LE, UTF-32BE）。

use encoding_rs::Encoding;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiByteEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
}

impl MultiByteEncoding {
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Utf32Le => "utf-32le",
            Self::Utf32Be => "utf-32be",
        }
    }

    pub fn whatwg_encoding(self) -> &'static Encoding {
        match self {
            Self::Utf8 => encoding_rs::UTF_8,
            Self::Utf16Le | Self::Utf32Le => encoding_rs::UTF_16LE,
            Self::Utf16Be | Self::Utf32Be => encoding_rs::UTF_16BE,
        }
    }
}

/// 检查明确的 BOM 标记。
pub fn detect_bom(bytes: &[u8]) -> Option<(MultiByteEncoding, usize)> {
    if bytes.len() >= 4 {
        if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
            return Some((MultiByteEncoding::Utf32Le, 4));
        }
        if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
            return Some((MultiByteEncoding::Utf32Be, 4));
        }
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some((MultiByteEncoding::Utf8, 3));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some((MultiByteEncoding::Utf16Le, 2));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some((MultiByteEncoding::Utf16Be, 2));
    }
    None
}

/// 无 BOM 的多字节编码启发式探测（基于 0 字节周期性与步进特征）。
pub fn detect_bomless_multibyte(bytes: &[u8]) -> Option<MultiByteEncoding> {
    if bytes.len() < 16 {
        return None;
    }
    let sample_len = bytes.len().min(1024);
    let sample = &bytes[..sample_len];

    // 1. 检查 UTF-32 模式（每 4 字节块中至少有连续高位零字节）
    let chunks = sample.len() / 4;
    if chunks >= 4 {
        let mut le32 = 0;
        let mut be32 = 0;
        for i in 0..chunks {
            let b0 = sample[i * 4];
            let b1 = sample[i * 4 + 1];
            let b2 = sample[i * 4 + 2];
            let b3 = sample[i * 4 + 3];
            if b2 == 0 && b3 == 0 && (b0 != 0 || b1 != 0) {
                le32 += 1;
            } else if b0 == 0 && b1 == 0 && (b2 != 0 || b3 != 0) {
                be32 += 1;
            }
        }
        if le32 * 10 >= chunks * 7 {
            return Some(MultiByteEncoding::Utf32Le);
        }
        if be32 * 10 >= chunks * 7 {
            return Some(MultiByteEncoding::Utf32Be);
        }
    }

    // 2. 检查 UTF-16 模式（奇偶位零字节高频交替）
    let pairs = sample.len() / 2;
    if pairs < 4 {
        return None;
    }

    let mut even_zeros = 0;
    let mut odd_zeros = 0;
    let mut even_non_zeros = 0;
    let mut odd_non_zeros = 0;

    for i in 0..pairs {
        let b0 = sample[i * 2];
        let b1 = sample[i * 2 + 1];
        if b0 == 0 {
            even_zeros += 1;
        } else {
            even_non_zeros += 1;
        }
        if b1 == 0 {
            odd_zeros += 1;
        } else {
            odd_non_zeros += 1;
        }
    }

    // UTF-16LE: b1 (高位) 绝大多数是 0 (针对 ASCII/Latin)，b0 是有效内容
    if odd_zeros * 10 >= pairs * 6 && even_non_zeros * 10 >= pairs * 6 && even_zeros < pairs / 4 {
        return Some(MultiByteEncoding::Utf16Le);
    }

    // UTF-16BE: b0 (高位) 绝大多数是 0，b1 是有效内容
    if even_zeros * 10 >= pairs * 6 && odd_non_zeros * 10 >= pairs * 6 && odd_zeros < pairs / 4 {
        return Some(MultiByteEncoding::Utf16Be);
    }

    // 3. 检查基于空白符与分隔符的 UTF-16 跨语言模式（适用于非拉丁语系，如阿语、希腊语、泰语、中文）
    let mut le_delims = 0;
    let mut be_delims = 0;
    for i in 0..pairs {
        let b0 = sample[i * 2];
        let b1 = sample[i * 2 + 1];
        if matches!(b0, 0x20 | 0x0A | 0x0D | 0x09) && b1 == 0x00 {
            le_delims += 1;
        } else if b0 == 0x00 && matches!(b1, 0x20 | 0x0A | 0x0D | 0x09) {
            be_delims += 1;
        }
    }
    if le_delims >= 3 && be_delims == 0 {
        return Some(MultiByteEncoding::Utf16Le);
    }
    if be_delims >= 3 && le_delims == 0 {
        return Some(MultiByteEncoding::Utf16Be);
    }

    None
}
