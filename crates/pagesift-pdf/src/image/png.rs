//! PNG 编码器（ISO/IEC 15948:2004，即 W3C PNG 第二版）：只写导出需要的部分——
//! IHDR、PLTE、一个 IDAT、IEND，扫描线一律用过滤类型 0（None），压缩用 zlib（RFC 1950/1951，
//! miniz_oxide）。块的 CRC 按规范附录 D 的 CRC-32（ISO 3309 / ITU-T V.42，多项式 0xEDB88320）。

/// PNG 色彩类型（规范 §11.2.2 表 11.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorType {
    Gray = 0,
    Rgb = 2,
    Palette = 3, // [协议] PNG 块布局与 CRC-32（ISO/IEC 15948）
}

/// [协议] PNG 文件签名（规范 §5.2）。
pub(crate) const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// CRC-32 查找表（规范附录 D 的参考实现）。
// [协议] CRC-32 表与块头（ISO/IEC 15948 附录 D）
const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256]; // [协议] PNG 块布局与 CRC-32（ISO/IEC 15948）
    let mut n = 0;
    // [协议] CRC-32 表与块头（ISO/IEC 15948 附录 D）
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        // [协议] CRC-32 表与块头（ISO/IEC 15948 附录 D）
        while k < 8 {
            // [协议] 反射多项式 0xEDB88320（ISO 3309）
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1) // [协议] PNG
            } else {
                c >> 1
            };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
};

/// 一段数据的 CRC-32。
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = u32::MAX;
    for p in parts {
        for &b in *p {
            c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8); // [协议] PNG 块布局与 CRC-32（ISO/IEC 15948）
        }
    }
    c ^ u32::MAX
}

// [协议] CRC-32 表与块头（ISO/IEC 15948 附录 D）
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// 把打包好的扫描线编成 PNG。`rows` 是 `height` 行、每行 `row_bytes` 字节的样本（规范 §7.2 的
/// 打包方式，和 PDF 图像数据的行打包相同：每行从字节边界开始）。`palette` 是 RGB 三元组。
pub(crate) fn encode(
    width: u32,
    height: u32,
    depth: u8,
    color: ColorType,
    palette: Option<&[u8]>,
    rows: &[u8],
    row_bytes: usize,
) -> Vec<u8> {
    let mut raw = Vec::with_capacity((row_bytes + 1) * height as usize);
    for r in 0..height as usize {
        raw.push(0); // 过滤类型 0：None（规范 §9.2）
        let start = (r * row_bytes).min(rows.len());
        let end = (start + row_bytes).min(rows.len());
        raw.extend_from_slice(&rows[start..end]);
        raw.resize(raw.len() + row_bytes - (end - start), 0);
    }
    // [协议] zlib 的默认压缩级别 6（zlib 手册 Z_DEFAULT_COMPRESSION）
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::with_capacity(13); // [协议] PNG 块布局与 CRC-32（ISO/IEC 15948）
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 位深、色彩类型；压缩方法 0、过滤方法 0、不隔行（规范 §11.2.2）
    ihdr.extend_from_slice(&[depth, color as u8, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    if let Some(p) = palette {
        chunk(&mut out, b"PLTE", p);
    }
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 按规范逐块检查：签名、每块长度和 CRC、IHDR 在最前、IEND 在最后；返回 (块类型, 数据)。
    pub(crate) fn parse(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], &SIGNATURE);
        let mut i = 8;
        let mut out = Vec::new();
        while i < png.len() {
            let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[i + 4..i + 8].try_into().unwrap();
            let data = &png[i + 8..i + 8 + len];
            let crc = u32::from_be_bytes(png[i + 8 + len..i + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&[&kind, data]), "CRC of {:?}", kind);
            out.push((kind, data.to_vec()));
            i += 12 + len;
        }
        assert_eq!(&out[0].0, b"IHDR");
        assert_eq!(&out.last().unwrap().0, b"IEND");
        out
    }

    /// 解出 IDAT 里的扫描线（去掉每行的过滤字节，过滤类型必须是 0）。
    pub(crate) fn scanlines(png: &[u8], row_bytes: usize) -> Vec<u8> {
        let idat: Vec<u8> = parse(png)
            .into_iter()
            .filter(|(k, _)| k == b"IDAT")
            .flat_map(|(_, d)| d)
            .collect();
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).unwrap();
        raw.chunks(row_bytes + 1)
            .flat_map(|r| {
                assert_eq!(r[0], 0);
                r[1..].to_vec()
            })
            .collect()
    }

    #[test]
    fn crc_matches_known_values() {
        // CRC-32 的标准校验值（"123456789" → 0xCBF43926）和 IEND 块的固定 CRC
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
    }

    #[test]
    fn rgb_roundtrip() {
        let rows = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9];
        let png = encode(2, 2, 8, ColorType::Rgb, None, &rows, 6);
        let chunks = parse(&png);
        assert_eq!(chunks[0].1, [0, 0, 0, 2, 0, 0, 0, 2, 8, 2, 0, 0, 0]);
        assert_eq!(scanlines(&png, 6), rows);
    }

    #[test]
    fn short_data_is_padded() {
        let png = encode(8, 3, 1, ColorType::Gray, None, &[0b1010_1010], 1);
        assert_eq!(scanlines(&png, 1), [0b1010_1010, 0, 0]);
    }

    #[test]
    fn palette_chunk_comes_before_idat() {
        let png = encode(1, 1, 8, ColorType::Palette, Some(&[1, 2, 3]), &[0], 1);
        let kinds: Vec<[u8; 4]> = parse(&png).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [*b"IHDR", *b"PLTE", *b"IDAT", *b"IEND"]);
    }
}
