//! 流过滤器（ISO 32000-1 §7.4）：Flate、LZW、ASCII85、ASCIIHex、RunLength、Brotli，以及 PNG/TIFF 预测器。
//!
//! 每一层的输出都有上限（防解压炸弹）。数据损坏时尽量保留已经正确解出的部分，
//! 与 pdf.js、MuPDF 的做法一致；图像专用的过滤器（DCT、JPX、CCITT、JBIG2）不解，调用方只取文字。

use super::{Dictionary, Object};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// 某一层的输出超过上限。
    TooLarge,
    /// 不支持的过滤器（图像编码或未知名字）。
    Unsupported,
    /// 数据坏到一个字节也解不出。
    Corrupt,
}

/// 依次应用过滤器。`filters` 里每项是过滤器名和它的参数字典。
pub(crate) fn decode(
    raw: &[u8],
    filters: &[(&[u8], Option<&Dictionary>)],
    limit: usize,
) -> Result<Vec<u8>, DecodeError> {
    if filters.is_empty() {
        if raw.len() > limit {
            return Err(DecodeError::TooLarge);
        }
        return Ok(raw.to_vec());
    }
    let mut buf: Option<Vec<u8>> = None;
    for &(name, params) in filters {
        let input = buf.as_deref().unwrap_or(raw);
        let out = match name {
            b"FlateDecode" | b"Fl" => predict(inflate(input, limit)?, params)?,
            b"LZWDecode" | b"LZW" => predict(lzw(input, early_change(params), limit)?, params)?,
            b"ASCII85Decode" | b"A85" => ascii85(input, limit)?,
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(input, limit)?,
            b"RunLengthDecode" | b"RL" => run_length(input, limit)?,
            b"BrotliDecode" => brotli(input, limit)?,
            // Crypt 过滤器只在 Identity（不另加密）时会走到这里；真正的解密在取数据时已经做了
            b"Crypt" => input.to_vec(),
            _ => return Err(DecodeError::Unsupported),
        };
        if out.len() > limit {
            return Err(DecodeError::TooLarge);
        }
        buf = Some(out);
    }
    Ok(buf.unwrap_or_default())
}

fn int_param(params: Option<&Dictionary>, key: &[u8], default: i64) -> i64 {
    params
        .and_then(|p| p.get(key))
        .and_then(Object::as_i64)
        .unwrap_or(default)
}

fn early_change(params: Option<&Dictionary>) -> bool {
    int_param(params, b"EarlyChange", 1) != 0
}

/// zlib 格式；校验和错误或数据截断时保留已经解出的部分。
/// 一个字节都解不出时按裸 deflate（跳过两字节头）再试一次：有些加密文件的 zlib 头和校验和是坏的。
fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let out = inflate_with(input, true, limit)?;
    if out.is_empty() && input.len() > 2 {
        return inflate_with(&input[2..], false, limit);
    }
    Ok(out)
}

fn inflate_with(input: &[u8], zlib: bool, limit: usize) -> Result<Vec<u8>, DecodeError> {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

    let mut flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
        | inflate_flags::TINFL_FLAG_HAS_MORE_INPUT;
    if zlib {
        flags |=
            inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER | inflate_flags::TINFL_FLAG_IGNORE_ADLER32;
    }
    let mut state = Box::<DecompressorOxide>::default();
    let cap = limit.saturating_add(1);
    let mut out = vec![0u8; input.len().saturating_mul(3).max(1024).min(cap)];
    let mut in_pos = 0;
    let mut out_pos = 0;
    loop {
        // 输入给完以后去掉“还有输入”标志，让解码器知道数据到头了
        let f = if in_pos >= input.len() {
            flags & !inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
        } else {
            flags
        };
        let (status, read, written) =
            decompress(&mut state, &input[in_pos..], &mut out, out_pos, f);
        in_pos += read;
        out_pos += written;
        match status {
            TINFLStatus::HasMoreOutput => {
                if out_pos > limit {
                    return Err(DecodeError::TooLarge);
                }
                // 翻倍，但总长不超过上限 + 1（再多一个字节就足以判定超限）
                let len = out.len().saturating_mul(2).min(cap);
                out.resize(len, 0);
            }
            TINFLStatus::NeedsMoreInput if in_pos < input.len() => {}
            // 完成、输入截断或数据损坏：都保留已经解出的部分
            _ => break,
        }
    }
    if out_pos > limit {
        return Err(DecodeError::TooLarge);
    }
    out.truncate(out_pos);
    Ok(out)
}

/// LZW（9–12 位码，256 清表，257 结束）。EarlyChange=1 时码宽提前一个码切换（PDF 默认）。
fn lzw(input: &[u8], early: bool, limit: usize) -> Result<Vec<u8>, DecodeError> {
    const CLEAR: usize = 256;
    const EOD: usize = 257;
    // 字典：每项是前缀码和末字节；长度另存，便于倒序展开
    let mut prefix: Vec<u16> = Vec::with_capacity(4096);
    let mut suffix: Vec<u8> = Vec::with_capacity(4096);
    let mut length: Vec<u16> = Vec::with_capacity(4096);
    let reset = |prefix: &mut Vec<u16>, suffix: &mut Vec<u8>, length: &mut Vec<u16>| {
        prefix.clear();
        suffix.clear();
        length.clear();
        for b in 0..256u16 {
            prefix.push(0);
            suffix.push(b as u8);
            length.push(1);
        }
        // 256、257 占位
        for _ in 0..2 {
            prefix.push(0);
            suffix.push(0);
            length.push(0);
        }
    };
    reset(&mut prefix, &mut suffix, &mut length);
    let mut out = Vec::with_capacity(input.len().saturating_mul(2).min(limit));
    let mut width = 9u32;
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut prev: Option<usize> = None;
    let mut tmp = Vec::new();
    for &byte in input {
        acc = acc << 8 | u32::from(byte);
        bits += 8;
        while bits >= width {
            let code = ((acc >> (bits - width)) & ((1 << width) - 1)) as usize;
            bits -= width;
            if code == CLEAR {
                reset(&mut prefix, &mut suffix, &mut length);
                width = 9;
                prev = None;
                continue;
            }
            if code == EOD {
                return Ok(out);
            }
            let next = prefix.len();
            let first;
            if code < next && length[code] > 0 {
                expand(code, &prefix, &suffix, &mut tmp);
                first = tmp[0];
                out.extend_from_slice(&tmp);
            } else if code == next
                && let Some(p) = prev
            {
                // KwKwK：新码就是“前一个串 + 它自己的首字节”
                expand(p, &prefix, &suffix, &mut tmp);
                first = tmp[0];
                tmp.push(first);
                out.extend_from_slice(&tmp);
            } else {
                // 非法码：停在这里，保留已解出的部分
                return Ok(out);
            }
            if out.len() > limit {
                return Err(DecodeError::TooLarge);
            }
            if let Some(p) = prev
                && next < 4096
            {
                prefix.push(p as u16);
                suffix.push(first);
                length.push(length[p] + 1);
            }
            prev = Some(code);
            let size = prefix.len() + usize::from(early);
            width = if size >= 2048 {
                12
            } else if size >= 1024 {
                11
            } else if size >= 512 {
                10
            } else {
                9
            };
        }
        acc &= (1 << bits) - 1;
    }
    Ok(out)
}

fn expand(mut code: usize, prefix: &[u16], suffix: &[u8], out: &mut Vec<u8>) {
    out.clear();
    // 长度不超过字典大小，链一定会走到单字节码
    for _ in 0..4096 {
        out.push(suffix[code]);
        if code < 256 {
            break;
        }
        code = usize::from(prefix[code]);
    }
    out.reverse();
}

fn ascii85(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(input.len() * 4 / 5 + 4);
    let mut group = [0u8; 5];
    let mut n = 0;
    let input = match input.windows(2).position(|w| w == b"~>") {
        Some(end) => &input[..end],
        None => input,
    };
    for &c in input {
        if out.len() > limit {
            return Err(DecodeError::TooLarge);
        }
        match c {
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = c - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |v, &d| v * 85 + u64::from(d));
                    if v > u64::from(u32::MAX) {
                        return if out.is_empty() {
                            Err(DecodeError::Corrupt)
                        } else {
                            Ok(out)
                        };
                    }
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            c if super::parse::is_whitespace(c) => {}
            // 其他字符（含组中间的 z）：数据到此为止
            _ => break,
        }
    }
    if n > 1 {
        for d in group.iter_mut().skip(n) {
            *d = 84;
        }
        let v = group.iter().fold(0u64, |v, &d| v * 85 + u64::from(d));
        if v <= u64::from(u32::MAX) {
            out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
        }
    }
    Ok(out)
}

fn ascii_hex(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(input.len() / 2 + 1);
    let mut high: Option<u8> = None;
    for &c in input {
        if c == b'>' {
            break;
        }
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            c if super::parse::is_whitespace(c) => continue,
            // 规范里非法字符是错误：到此为止，保留已解出的部分（跳过它继续解只会得到乱码）
            _ => break,
        };
        match high.take() {
            Some(h) => out.push(h << 4 | v),
            None => high = Some(v),
        }
        if out.len() > limit {
            return Err(DecodeError::TooLarge);
        }
    }
    if let Some(h) = high {
        out.push(h << 4);
    }
    Ok(out)
}

fn run_length(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if out.len() > limit {
            return Err(DecodeError::TooLarge);
        }
        let n = input[i];
        i += 1;
        match n {
            128 => break,
            0..=127 => {
                let end = (i + usize::from(n) + 1).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            _ => {
                let Some(&b) = input.get(i) else { break };
                out.resize(out.len() + 257 - usize::from(n), b);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn brotli(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    use std::io::Read;
    let mut out = Vec::new();
    let reader = brotli_decompressor::Decompressor::new(input, 4096);
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| DecodeError::Corrupt)?;
    Ok(out)
}

/// 预测器（Flate/LZW 的 DecodeParms）：2 = TIFF 横向差分，10–15 = PNG 逐行过滤。
fn predict(data: Vec<u8>, params: Option<&Dictionary>) -> Result<Vec<u8>, DecodeError> {
    let predictor = int_param(params, b"Predictor", 1);
    if predictor != 2 && !(10..=15).contains(&predictor) {
        return Ok(data);
    }
    let positive = |key: &[u8], default: i64| int_param(params, key, default).max(1) as usize;
    let columns = positive(b"Columns", 1);
    let colors = positive(b"Colors", 1);
    let bits = positive(b"BitsPerComponent", 8);
    let row = columns
        .checked_mul(colors)
        .and_then(|v| v.checked_mul(bits))
        .ok_or(DecodeError::Corrupt)?
        .div_ceil(8);
    if predictor == 2 {
        return Ok(tiff(data, row, colors, bits));
    }
    Ok(png(&data, row, (colors * bits).div_ceil(8)))
}

fn png(data: &[u8], row: usize, bpp: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    let mut cur = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        let kind = chunk[0];
        let body = &chunk[1..];
        let len = body.len();
        cur[..len].copy_from_slice(body);
        let bpp = bpp.min(len.max(1));
        match kind {
            0 => {}
            1 => {
                for i in bpp..len {
                    cur[i] = cur[i].wrapping_add(cur[i - bpp]);
                }
            }
            2 => {
                for i in 0..len {
                    cur[i] = cur[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                for i in 0..len {
                    let left = if i >= bpp { u16::from(cur[i - bpp]) } else { 0 };
                    cur[i] = cur[i].wrapping_add(((left + u16::from(prev[i])) / 2) as u8);
                }
            }
            4 => {
                for i in 0..len {
                    let (left, upleft) = if i >= bpp {
                        (cur[i - bpp], prev[i - bpp])
                    } else {
                        (0, 0)
                    };
                    cur[i] = cur[i].wrapping_add(paeth(left, prev[i], upleft));
                }
            }
            // 未知的行类型：后面的数据不可信，到此为止
            _ => break,
        }
        out.extend_from_slice(&cur[..len]);
        std::mem::swap(&mut prev, &mut cur);
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// TIFF 预测器 2：每个分量是同一行里前一个同色分量加上差值（模 2^bits）。
fn tiff(mut data: Vec<u8>, row: usize, colors: usize, bits: usize) -> Vec<u8> {
    for line in data.chunks_mut(row) {
        match bits {
            8 => {
                for i in colors..line.len() {
                    line[i] = line[i].wrapping_add(line[i - colors]);
                }
            }
            16 => {
                let samples = line.len() / 2;
                for s in colors..samples {
                    let prev =
                        u16::from_be_bytes([line[(s - colors) * 2], line[(s - colors) * 2 + 1]]);
                    let cur = u16::from_be_bytes([line[s * 2], line[s * 2 + 1]]);
                    line[s * 2..s * 2 + 2].copy_from_slice(&cur.wrapping_add(prev).to_be_bytes());
                }
            }
            1 | 2 | 4 => {
                let mask = (1u16 << bits) - 1;
                let samples = line.len() * 8 / bits;
                let mut acc = vec![0u16; colors];
                for s in 0..samples {
                    let bit = s * bits;
                    let shift = 8 - bits - bit % 8;
                    let byte = &mut line[bit / 8];
                    let v = (u16::from(*byte) >> shift) & mask;
                    let c = s % colors;
                    acc[c] = (acc[c] + v) & mask;
                    *byte = (*byte & !((mask as u8) << shift)) | ((acc[c] as u8) << shift);
                }
            }
            _ => break,
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flate(data: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
    }

    #[test]
    fn flate_roundtrip_and_limit() {
        let text = b"BT /F1 12 Tf (Hello) Tj ET ".repeat(500);
        let z = flate(&text);
        assert_eq!(
            decode(&z, &[(b"FlateDecode", None)], 1 << 20).unwrap(),
            text
        );
        assert_eq!(
            decode(&z, &[(b"FlateDecode", None)], 100),
            Err(DecodeError::TooLarge)
        );
        // 截断：保留已解出的前缀
        let part = decode(&z[..z.len() / 2], &[(b"FlateDecode", None)], 1 << 20).unwrap();
        assert!(!part.is_empty() && text.starts_with(&part));
    }

    #[test]
    fn lzw_spec_example() {
        // ISO 32000-1 §7.4.4.2 的例子
        let input = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&input, true, 1 << 20).unwrap(), b"-----A---B");
    }

    #[test]
    fn ascii_filters() {
        assert_eq!(
            ascii85(b"87cURD]i,\"Ebo80~>", 100).unwrap(),
            b"Hello World!"
        );
        assert_eq!(ascii85(b"z~>", 100).unwrap(), [0, 0, 0, 0]);
        assert_eq!(ascii_hex(b"48 65 6c 6C 6>", 100).unwrap(), b"Hell`");
        assert_eq!(
            run_length(&[2, b'a', b'b', b'c', 254, b'x', 128], 100).unwrap(),
            b"abcxxx"
        );
    }

    #[test]
    fn png_up_predictor() {
        // 两行、每行 3 字节：第二行用 Up
        let data = [0, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(png(&data, 3, 1), [1, 2, 3, 2, 3, 4]);
    }
}
