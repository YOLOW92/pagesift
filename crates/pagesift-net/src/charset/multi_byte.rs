//! 纯 Rust 多字节编码识别：BOM 与无 BOM 探测（UTF-16LE, UTF-16BE, UTF-32LE, UTF-32BE）。

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE};

/// 检查明确的 BOM 标记。
pub fn detect_bom(bytes: &[u8]) -> Option<(&'static Encoding, usize)> {
    if let Some((enc, len)) = Encoding::for_bom(bytes) {
        return Some((enc, len));
    }
    // 处理 UTF-32 签名（encoding_rs 不原生支持 UTF-32 解码，但探测层可识别）
    if bytes.len() >= 4 {
        if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
            return Some((UTF_16LE, 4));
        }
        if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
            return Some((UTF_16BE, 4));
        }
    }
    None
}

/// 无 BOM 的多字节编码启发式探测（基于 0 字节周期性分布）。
pub fn detect_bomless_multibyte(bytes: &[u8]) -> Option<&'static Encoding> {
    if bytes.len() < 8 {
        return None;
    }
    let sample_len = bytes.len().min(1024);
    let sample = &bytes[..sample_len];

    // 1. 检查 UTF-32 模式（3 个连续零字节）
    if sample.len() >= 16 && sample.len() % 4 == 0 {
        let mut le32 = 0;
        let mut be32 = 0;
        let chunks = sample.len() / 4;
        for i in 0..chunks {
            let chunk = &sample[i * 4..(i + 1) * 4];
            if chunk[0] != 0 && chunk[1] == 0 && chunk[2] == 0 && chunk[3] == 0 {
                le32 += 1;
            } else if chunk[0] == 0 && chunk[1] == 0 && chunk[2] == 0 && chunk[3] != 0 {
                be32 += 1;
            }
        }
        if le32 * 10 >= chunks * 7 {
            return Some(UTF_16LE);
        }
        if be32 * 10 >= chunks * 7 {
            return Some(UTF_16BE);
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
        return Some(UTF_16LE);
    }

    // UTF-16BE: b0 (高位) 绝大多数是 0，b1 是有效内容
    if even_zeros * 10 >= pairs * 6 && odd_non_zeros * 10 >= pairs * 6 && odd_zeros < pairs / 4 {
        return Some(UTF_16BE);
    }

    None
}
