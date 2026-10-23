//! 纯 Rust 遗留编码与特殊多字节格式解码器（UTF-32、UTF-7、EBCDIC CP500、DOS CP437）。
//!
//! 补全 WHATWG 规范以外的工业历史编码，为 PageSift 提供全向字符集解码与识别能力。

/// IBM CP500 EBCDIC 到 Unicode 码点映射表（256 元素完整表）。
pub const CP500_TABLE: [u16; 256] = [
    0x0000, 0x0001, 0x0002, 0x0003, 0x009C, 0x0009, 0x0086, 0x007F, 0x0097, 0x008D, 0x008E, 0x000B,
    0x000C, 0x000D, 0x000E, 0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x009D, 0x0085, 0x0008, 0x0087,
    0x0018, 0x0019, 0x0092, 0x008F, 0x001C, 0x001D, 0x001E, 0x001F, 0x0080, 0x0081, 0x0082, 0x0083,
    0x0084, 0x000A, 0x0017, 0x001B, 0x0088, 0x0089, 0x008A, 0x008B, 0x008C, 0x0005, 0x0006, 0x0007,
    0x0090, 0x0091, 0x0016, 0x0093, 0x0094, 0x0095, 0x0096, 0x0004, 0x0098, 0x0099, 0x009A, 0x009B,
    0x0014, 0x0015, 0x009E, 0x001A, 0x0020, 0x00A0, 0x00E2, 0x00E4, 0x00E0, 0x00E1, 0x00E3, 0x00E5,
    0x00E7, 0x00F1, 0x005B, 0x002E, 0x003C, 0x0028, 0x002B, 0x0021, 0x0026, 0x00E9, 0x00EA, 0x00EB,
    0x00E8, 0x00ED, 0x00EE, 0x00EF, 0x00EC, 0x00DF, 0x005D, 0x0024, 0x002A, 0x0029, 0x003B, 0x005E,
    0x002D, 0x002F, 0x00C2, 0x00C4, 0x00C0, 0x00C1, 0x00C3, 0x00C5, 0x00C7, 0x00D1, 0x00A6, 0x002C,
    0x0025, 0x005F, 0x003E, 0x003F, 0x00F8, 0x00C9, 0x00CA, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF,
    0x00CC, 0x0060, 0x003A, 0x0023, 0x0040, 0x0027, 0x003D, 0x0022, 0x00D8, 0x0061, 0x0062, 0x0063,
    0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x00AB, 0x00BB, 0x00F0, 0x00FD, 0x00FE, 0x00B1,
    0x00B0, 0x006A, 0x006B, 0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x00AA, 0x00BA,
    0x00E6, 0x00B8, 0x00C6, 0x00A4, 0x00B5, 0x007E, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077, 0x0078,
    0x0079, 0x007A, 0x00A1, 0x00BF, 0x00D0, 0x00DD, 0x00DE, 0x00AE, 0x00A2, 0x00A3, 0x00A5, 0x00B7,
    0x00A9, 0x00A7, 0x00B6, 0x00BC, 0x00BD, 0x00BE, 0x00AC, 0x007C, 0x00AF, 0x00A8, 0x00B4, 0x00D7,
    0x007B, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047, 0x0048, 0x0049, 0x00AD, 0x00F4,
    0x00F6, 0x00F2, 0x00F3, 0x00F5, 0x007D, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050,
    0x0051, 0x0052, 0x00B9, 0x00FB, 0x00FC, 0x00F9, 0x00FA, 0x00FF, 0x005C, 0x00F7, 0x0053, 0x0054,
    0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x00B2, 0x00D4, 0x00D6, 0x00D2, 0x00D3, 0x00D5,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x00B3, 0x00DB,
    0x00DC, 0x00D9, 0x00DA, 0x009F,
];

/// 纯 Rust 解码 UTF-32 Little-Endian。
pub fn decode_utf32_le(bytes: &[u8]) -> (String, bool) {
    let mut out = String::with_capacity(bytes.len() / 4);
    let mut had_errors = false;
    for chunk in bytes.chunks_exact(4) {
        let code = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if let Some(ch) = char::from_u32(code) {
            out.push(ch);
        } else {
            out.push('\u{FFFD}');
            had_errors = true;
        }
    }
    (out, had_errors)
}

/// 纯 Rust 解码 UTF-32 Big-Endian。
pub fn decode_utf32_be(bytes: &[u8]) -> (String, bool) {
    let mut out = String::with_capacity(bytes.len() / 4);
    let mut had_errors = false;
    for chunk in bytes.chunks_exact(4) {
        let code = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if let Some(ch) = char::from_u32(code) {
            out.push(ch);
        } else {
            out.push('\u{FFFD}');
            had_errors = true;
        }
    }
    (out, had_errors)
}

/// 检查是否为合法的 UTF-7 文本（RFC 2152）。
pub fn detect_utf7(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    // 所有字节必须在 ASCII 范围内
    if bytes.iter().any(|&b| b >= 0x80) {
        return false;
    }
    // 1. 检查明确的 UTF-7 BOM
    if bytes.starts_with(b"+/v8")
        || bytes.starts_with(b"+/v9")
        || bytes.starts_with(b"+/v+")
        || bytes.starts_with(b"+/v/")
    {
        return true;
    }
    // 2. 检查 `+<Base64>-` 或 `+<Base64>` 模式且解码出有效字符
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'-' {
                i += 1;
                continue;
            }
            let start = i;
            let mut buf: u32 = 0;
            let mut bits = 0;
            let mut found_non_ascii = false;
            while i < bytes.len() {
                let v = match bytes[i] {
                    b'A'..=b'Z' => bytes[i] - b'A',
                    b'a'..=b'z' => bytes[i] - b'a' + 26,
                    b'0'..=b'9' => bytes[i] - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => break,
                };
                buf = (buf << 6) | (v as u32);
                bits += 6;
                if bits >= 16 {
                    bits -= 16;
                    let u16_val = ((buf >> bits) & 0xFFFF) as u16;
                    if u16_val >= 0x80 {
                        found_non_ascii = true;
                    }
                }
                i += 1;
            }
            if i > start && found_non_ascii {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

/// 纯 Rust 解码 UTF-7 文本（RFC 2152）。
pub fn decode_utf7(bytes: &[u8]) -> (String, bool) {
    let mut out = String::with_capacity(bytes.len());
    let mut had_errors = false;
    let mut i = 0;
    let n = bytes.len();

    while i < n {
        if bytes[i] == b'+' {
            i += 1;
            if i < n && bytes[i] == b'-' {
                out.push('+');
                i += 1;
                continue;
            }
            let mut buf: u32 = 0;
            let mut bits = 0;
            let mut u16_buf = Vec::new();

            while i < n {
                let v = match bytes[i] {
                    b'A'..=b'Z' => bytes[i] - b'A',
                    b'a'..=b'z' => bytes[i] - b'a' + 26,
                    b'0'..=b'9' => bytes[i] - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => break,
                };
                buf = (buf << 6) | (v as u32);
                bits += 6;
                if bits >= 16 {
                    bits -= 16;
                    u16_buf.push(((buf >> bits) & 0xFFFF) as u16);
                }
                i += 1;
            }
            if i < n && bytes[i] == b'-' {
                i += 1;
            }
            for ch_res in char::decode_utf16(u16_buf) {
                match ch_res {
                    Ok(c) => {
                        // 过滤掉 UTF-7 BOM (U+FEFF)
                        if c != '\u{FEFF}' {
                            out.push(c);
                        }
                    }
                    Err(_) => {
                        out.push('\u{FFFD}');
                        had_errors = true;
                    }
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    (out, had_errors)
}

/// 启发式识别 EBCDIC 代码页（以 IBM CP500 为主）。
pub fn detect_ebcdic(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    let sample = &bytes[..bytes.len().min(4096)];
    let s40 = sample.iter().filter(|&&b| b == 0x40).count();
    let s20 = sample.iter().filter(|&&b| b == 0x20).count();
    // EBCDIC 中空格为 0x40，出现率通常在 5%~25%，且 ASCII 空格 0x20 极低
    s40 * 20 >= sample.len() && s40 > s20 * 5 && sample[0] != 0
}

/// 纯 Rust 解码 IBM CP500 EBCDIC。
pub fn decode_ebcdic_cp500(bytes: &[u8]) -> (String, bool) {
    let mut out = String::with_capacity(bytes.len());
    let mut had_errors = false;
    for &b in bytes {
        let code = CP500_TABLE[b as usize];
        if let Some(ch) = char::from_u32(code as u32) {
            out.push(ch);
        } else {
            out.push('\u{FFFD}');
            had_errors = true;
        }
    }
    (out, had_errors)
}

/// 检查是否包含 DOS CP437 特有的制表符模式（仅在 ASCII 背景下的艺术字/边框）。
pub fn detect_cp437_box(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(2048)];
    let box_count = sample
        .iter()
        .filter(|&&b| (0xB0..=0xDF).contains(&b))
        .count();
    let ascii_printable = sample
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .count();
    box_count >= 30 && ascii_printable * 2 >= sample.len()
}
