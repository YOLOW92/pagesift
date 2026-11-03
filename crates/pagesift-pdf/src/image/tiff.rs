//! CCITT 传真码流套 TIFF 文件头（TIFF 6.0 第 2、3、11 节）：码流原样放进单条带，
//! 压缩方式按 /K 选 T.4（Compression 3，ITU-T T.4）或 T.6（Compression 4，ITU-T T.6）。
//!
//! 颜色：TIFF 的传真解码器把黑色游程解成 1，PhotometricInterpretation 0（WhiteIsZero）
//! 时 1 显示为黑。PDF 的 CCITTFaxDecode 在 BlackIs1 为假（默认）时把黑色游程解成 0，
//! 再经 /Decode [0 1] 和 DeviceGray（或模板蒙版）显示为黑。所以只有 BlackIs1 与
//! /Decode [1 0] 恰有一个成立时显示是反的，那时用 BlackIsZero（1）。

use crate::file::{Dictionary, Object};

/// CCITT 码流的参数（ISO 32000-2 §7.4.6 表 11）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Ccitt {
    /// [协议] /K：<0 纯二维（T.6），0 纯一维（T.4 MH），>0 混合（T.4 MR）。缺省 0。
    pub(crate) k: i64,
    pub(crate) columns: u32,
    pub(crate) rows: u32,
    pub(crate) black_is_1: bool,
    pub(crate) byte_align: bool,
}

impl Ccitt {
    /// 读参数字典；`height` 是图像的 /Height（/Rows 缺省或为 0 时用它）。
    pub(crate) fn from_params(p: Option<&Dictionary>, height: u32) -> Ccitt {
        let int = |k: &[u8]| p.and_then(|d| d.get(k)).and_then(Object::as_i64);
        let flag = |k: &[u8]| matches!(p.and_then(|d| d.get(k)), Some(Object::Boolean(true)));
        // [协议] /Columns 缺省 1728（ISO 32000-2 §7.4.6 表 11，A4 传真的像素宽度）
        let columns = int(b"Columns").filter(|&c| c > 0).unwrap_or(1728);
        let rows = int(b"Rows").filter(|&r| r > 0).unwrap_or(i64::from(height));
        Ccitt {
            k: int(b"K").unwrap_or(0),
            columns: columns.clamp(1, i64::from(u32::MAX)) as u32,
            rows: rows.clamp(0, i64::from(u32::MAX)) as u32,
            black_is_1: flag(b"BlackIs1"),
            byte_align: flag(b"EncodedByteAlign"),
        }
    }
}

// [协议] TIFF 6.0 的标签号（第 8 节、第 11 节）和字段类型（SHORT 3、LONG 4、RATIONAL 5）
const IMAGE_WIDTH: u16 = 256;
const IMAGE_LENGTH: u16 = 257; // [协议] TIFF 6.0 文件头与 IFD 布局
const BITS_PER_SAMPLE: u16 = 258; // [协议] TIFF 6.0 文件头与 IFD 布局
const COMPRESSION: u16 = 259; // [协议] TIFF 6.0 文件头与 IFD 布局
const PHOTOMETRIC: u16 = 262; // [协议] TIFF 6.0 文件头与 IFD 布局
const STRIP_OFFSETS: u16 = 273; // [协议] TIFF 6.0 文件头与 IFD 布局
const SAMPLES_PER_PIXEL: u16 = 277; // [协议] TIFF 6.0 文件头与 IFD 布局
const ROWS_PER_STRIP: u16 = 278; // [协议] TIFF 6.0 文件头与 IFD 布局
const STRIP_BYTE_COUNTS: u16 = 279; // [协议] TIFF 6.0 文件头与 IFD 布局
const X_RESOLUTION: u16 = 282; // [协议] TIFF 6.0 文件头与 IFD 布局
const Y_RESOLUTION: u16 = 283; // [协议] TIFF 6.0 文件头与 IFD 布局
const T4_OPTIONS: u16 = 292; // [协议] TIFF 6.0 文件头与 IFD 布局
const T6_OPTIONS: u16 = 293; // [协议] TIFF 6.0 文件头与 IFD 布局
const RESOLUTION_UNIT: u16 = 296; // [协议] TIFF 6.0 文件头与 IFD 布局
const SHORT: u16 = 3; // [协议] TIFF 6.0 文件头与 IFD 布局
const LONG: u16 = 4; // [协议] TIFF 6.0 文件头与 IFD 布局
const RATIONAL: u16 = 5; // [协议] TIFF 6.0 文件头与 IFD 布局

/// 套上 TIFF 头。`inverted`：PDF 的 /Decode 是 [1 0]。
pub(crate) fn wrap(data: &[u8], p: &Ccitt, inverted: bool) -> Vec<u8> {
    // [协议] Compression：3 = CCITT T.4，4 = CCITT T.6（TIFF 6.0 第 11 节）
    let compression: u32 = if p.k < 0 { 4 } else { 3 };
    let photometric = u32::from(p.black_is_1 != inverted);
    // [协议] T4Options 位 0：二维编码；位 2：EOL 前补齐到字节边界（TIFF 6.0 第 11 节）
    let t4 = u32::from(p.k > 0) | if p.byte_align { 4 } else { 0 };
    let mut entries: Vec<(u16, u16, u32)> = vec![
        (IMAGE_WIDTH, LONG, p.columns),
        (IMAGE_LENGTH, LONG, p.rows),
        (BITS_PER_SAMPLE, SHORT, 1),
        (COMPRESSION, SHORT, compression),
        (PHOTOMETRIC, SHORT, photometric),
        (STRIP_OFFSETS, LONG, 0), // 下面填
        (SAMPLES_PER_PIXEL, SHORT, 1),
        (ROWS_PER_STRIP, LONG, p.rows),
        (STRIP_BYTE_COUNTS, LONG, data.len() as u32),
        (X_RESOLUTION, RATIONAL, 0), // 下面填偏移
        (Y_RESOLUTION, RATIONAL, 0),
    ];
    // [协议] TIFF Compression 3 = T.4
    entries.push(if compression == 3 {
        (T4_OPTIONS, LONG, t4)
    } else {
        (T6_OPTIONS, LONG, 0)
    });
    // [协议] ResolutionUnit 2：英寸
    entries.push((RESOLUTION_UNIT, SHORT, 2));
    entries.sort_by_key(|e| e.0); // 第 2 节：IFD 的条目按标签号升序
    // 布局：8 字节文件头 | IFD（2 + 12n + 4）| 两个 RATIONAL | 码流
    let ifd_len = 2 + 12 * entries.len() + 4; // [协议] TIFF 6.0 文件头与 IFD 布局
    let rational_at = 8 + ifd_len; // [协议] TIFF 6.0 文件头与 IFD 布局
    let data_at = rational_at + 16; // [协议] TIFF 6.0 文件头与 IFD 布局
    let mut out = Vec::with_capacity(data_at + data.len());
    // [协议] 小端文件头 "II"、42、第一个 IFD 的偏移（第 2 节）
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes()); // [协议] TIFF 6.0 文件头与 IFD 布局
    out.extend_from_slice(&8u32.to_le_bytes()); // [协议] TIFF 6.0 文件头与 IFD 布局
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, mut value) in entries {
        match tag {
            STRIP_OFFSETS => value = data_at as u32,
            X_RESOLUTION => value = rational_at as u32,
            Y_RESOLUTION => value = rational_at as u32 + 8, // [协议] TIFF 6.0 文件头与 IFD 布局
            _ => {}
        }
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        // 一个 SHORT 放在值域的低两个字节，其余补零（第 2 节）
        if kind == SHORT {
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // 没有下一个 IFD
    // [策略] PDF 不记录扫描分辨率，写 72 dpi：一个 PDF 点一个像素
    for _ in 0..2 {
        out.extend_from_slice(&72u32.to_le_bytes()); // [协议] TIFF 6.0 文件头与 IFD 布局
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 读回 IFD：(标签, 类型, 值)。检查文件头、条目升序、偏移都落在文件里。
    pub(crate) fn ifd(tif: &[u8]) -> Vec<(u16, u16, u32)> {
        assert_eq!(&tif[..4], b"II\x2a\x00");
        let at = u32::from_le_bytes(tif[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(tif[at..at + 2].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        for i in 0..n {
            let e = &tif[at + 2 + 12 * i..at + 14 + 12 * i];
            let tag = u16::from_le_bytes([e[0], e[1]]);
            let kind = u16::from_le_bytes([e[2], e[3]]);
            assert_eq!(u32::from_le_bytes(e[4..8].try_into().unwrap()), 1);
            let v = if kind == SHORT {
                u32::from(u16::from_le_bytes([e[8], e[9]]))
            } else {
                u32::from_le_bytes(e[8..12].try_into().unwrap())
            };
            out.push((tag, kind, v));
        }
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0));
        let next = &tif[at + 2 + 12 * n..at + 6 + 12 * n];
        assert_eq!(next, [0, 0, 0, 0]);
        out
    }

    fn value(e: &[(u16, u16, u32)], tag: u16) -> u32 {
        e.iter().find(|x| x.0 == tag).unwrap().2
    }

    #[test]
    fn t6_layout() {
        let p = Ccitt {
            k: -1,
            columns: 100,
            rows: 50,
            black_is_1: false,
            byte_align: false,
        };
        let data = [1u8, 2, 3, 4, 5];
        let tif = wrap(&data, &p, false);
        let e = ifd(&tif);
        assert_eq!(value(&e, COMPRESSION), 4);
        assert_eq!(value(&e, IMAGE_WIDTH), 100);
        assert_eq!(value(&e, IMAGE_LENGTH), 50);
        assert_eq!(value(&e, PHOTOMETRIC), 0);
        assert!(e.iter().any(|x| x.0 == T6_OPTIONS));
        let off = value(&e, STRIP_OFFSETS) as usize;
        let len = value(&e, STRIP_BYTE_COUNTS) as usize;
        assert_eq!(&tif[off..off + len], &data);
        let xr = value(&e, X_RESOLUTION) as usize;
        assert_eq!(&tif[xr..xr + 8], &[72, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn t4_options_and_inversion() {
        let p = Ccitt {
            k: 4,
            columns: 8,
            rows: 1,
            black_is_1: true,
            byte_align: true,
        };
        let e = ifd(&wrap(&[0], &p, false));
        assert_eq!(value(&e, COMPRESSION), 3);
        assert_eq!(value(&e, T4_OPTIONS), 5);
        assert_eq!(value(&e, PHOTOMETRIC), 1);
        // BlackIs1 与 /Decode [1 0] 同时成立：负负得正
        assert_eq!(value(&ifd(&wrap(&[0], &p, true)), PHOTOMETRIC), 0);
    }

    #[test]
    fn rows_default_to_image_height() {
        let p = Ccitt::from_params(None, 33);
        assert_eq!((p.k, p.columns, p.rows), (0, 1728, 33));
    }
}
