//! TIFF 6.0 第一页：无压缩、PackBits、LZW、Deflate、CCITT（G3、G4）、JPEG 的条带图。
//! 灰度、RGB、CMYK、调色板，1、4、8、16 位。分块（tile）的图报明确错误。

use super::super::{ccitt, cmyk_to_rgb, jpeg};
use super::lzw::{self, Order};
use super::{GrayImage, check_size, luma, over_white};
use crate::image::tiff::Ccitt;

struct Reader<'a> {
    b: &'a [u8],
    big: bool,
}

impl Reader<'_> {
    fn u16(&self, i: usize) -> Option<u16> {
        let s = self.b.get(i..i + 2)?;
        Some(if self.big {
            u16::from_be_bytes([s[0], s[1]])
        } else {
            u16::from_le_bytes([s[0], s[1]])
        })
    }

    fn u32(&self, i: usize) -> Option<u32> {
        let s = self.b.get(i..i + 4)?;
        Some(if self.big {
            u32::from_be_bytes([s[0], s[1], s[2], s[3]])
        } else {
            u32::from_le_bytes([s[0], s[1], s[2], s[3]])
        })
    }

    /// 一个目录项的全部值。放不下 4 字节的值在别处，偏移量写在项里。
    fn values(&self, entry: usize) -> Option<Vec<u32>> {
        let kind = self.u16(entry + 2)?;
        let count = self.u32(entry + 4)? as usize;
        let size = match kind {
            1 | 2 | 7 => 1,
            3 => 2,
            4 => 4,
            _ => return None,
        };
        // 声明的个数不可能超过文件本身
        if count.checked_mul(size)? > self.b.len() {
            return None;
        }
        let at = if count * size <= 4 {
            entry + 8
        } else {
            self.u32(entry + 8)? as usize
        };
        (0..count)
            .map(|i| match size {
                1 => self.b.get(at + i).map(|&v| u32::from(v)),
                2 => self.u16(at + i * 2).map(u32::from),
                _ => self.u32(at + i * 4),
            })
            .collect()
    }
}

pub(super) fn decode(b: &[u8], max_pixels: u64) -> Result<GrayImage, String> {
    let r = Reader {
        b,
        big: b.starts_with(b"MM"),
    };
    let short = || "TIFF is truncated".to_owned();
    let ifd = r.u32(4).ok_or_else(short)? as usize;
    let n = usize::from(r.u16(ifd).ok_or_else(short)?);
    let tag = |id: u16| -> Option<Vec<u32>> {
        (0..n).find_map(|i| {
            let e = ifd + 2 + i * 12;
            (r.u16(e)? == id).then(|| r.values(e)).flatten()
        })
    };
    let one = |id: u16, default: u32| tag(id).and_then(|v| v.first().copied()).unwrap_or(default);
    let width = one(256, 0);
    let height = one(257, 0);
    let total = check_size(width, height, max_pixels)?;
    if tag(322).is_some() {
        return Err("tiled TIFF images are not supported".into());
    }
    let samples = one(277, 1) as usize;
    let bits = one(258, 1) as usize;
    let compression = one(259, 1);
    let photometric = one(262, 1);
    let fill_reversed = one(266, 1) == 2;
    let predictor = one(317, 1);
    let rows_per_strip = one(278, height).clamp(1, height) as usize;
    if one(284, 1) != 1 && samples > 1 {
        return Err("planar TIFF images are not supported".into());
    }
    if !matches!(bits, 1 | 4 | 8 | 16) || !(1..=5).contains(&samples) {
        return Err(format!("TIFF with {samples}x{bits} bits is not supported"));
    }
    let offsets = tag(273).ok_or("TIFF has no strips")?;
    let counts = tag(279).ok_or("TIFF has no strip sizes")?;
    let (w, h) = (width as usize, height as usize);
    let row_bytes = (w * samples * bits).div_ceil(8);
    let mut pixels = vec![255u8; total];
    let mut y = 0usize;
    for (&off, &count) in offsets.iter().zip(&counts) {
        if y >= h {
            break;
        }
        let rows = rows_per_strip.min(h - y);
        let Some(raw) = b.get(off as usize..(off as usize).saturating_add(count as usize)) else {
            break; // 截断：其余留白
        };
        let reversed;
        let raw = if fill_reversed {
            reversed = raw.iter().map(|v| v.reverse_bits()).collect::<Vec<u8>>();
            &reversed[..]
        } else {
            raw
        };
        // CCITT 和 JPEG 直接出灰度
        match compression {
            2..=4 => {
                let k = match compression {
                    4 => -1,
                    3 if one(292, 0) & 1 == 1 => 1,
                    _ => 0,
                };
                let p = Ccitt {
                    k,
                    columns: width,
                    rows: rows as u32,
                    black_is_1: false,
                    byte_align: compression == 2,
                };
                let img = ccitt::decode(raw, &p, w * rows)?;
                // 解码器按“白 255、黑 0”；BlackIsZero 的传真图位值相反
                let invert = photometric == 1;
                blit(&mut pixels, w, y, &img.data, invert);
            }
            7 | 6 => {
                let joined;
                let data = match tag(347) {
                    Some(tables) if tables.len() > 4 => {
                        joined = join_jpeg(&tables, raw);
                        &joined[..]
                    }
                    _ => raw,
                };
                let img = jpeg::decode_gray(data, None, w.saturating_mul(rows).saturating_mul(4))?;
                let gray = if img.channels == 1 {
                    img
                } else {
                    img.to_gray()
                };
                blit(&mut pixels, w, y, &gray.data, false);
            }
            _ => {
                let limit = row_bytes.saturating_mul(rows);
                let mut data = inflate_strip(raw, compression, limit)?;
                if predictor == 2 && bits == 8 {
                    undo_predictor(&mut data, row_bytes, samples);
                }
                let map = tag(320);
                for row in 0..rows {
                    let Some(line) = data.get(row * row_bytes..(row + 1) * row_bytes) else {
                        break;
                    };
                    let out = &mut pixels[(y + row) * w..(y + row + 1) * w];
                    for (x, p) in out.iter_mut().enumerate() {
                        *p = gray_of(line, x, samples, bits, photometric, map.as_deref(), r.big);
                    }
                }
            }
        }
        y += rows;
    }
    Ok(GrayImage {
        width,
        height,
        pixels,
    })
}

fn blit(pixels: &mut [u8], w: usize, y: usize, data: &[u8], invert: bool) {
    let start = (y * w).min(pixels.len());
    let n = data.len().min(pixels.len() - start);
    for (d, s) in pixels[start..start + n].iter_mut().zip(data) {
        *d = if invert { 255 - *s } else { *s };
    }
}

/// JPEGTables（去掉结尾的 EOI）拼上条带（去掉开头的 SOI）。
fn join_jpeg(tables: &[u32], strip: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = tables.iter().map(|&v| v as u8).collect();
    if out.ends_with(&[0xFF, 0xD9]) {
        out.truncate(out.len() - 2);
    }
    out.extend_from_slice(strip.strip_prefix(&[0xFF, 0xD8]).unwrap_or(strip));
    out
}

fn inflate_strip(raw: &[u8], compression: u32, limit: usize) -> Result<Vec<u8>, String> {
    use miniz_oxide::inflate::decompress_to_vec_zlib_with_limit as zlib;
    match compression {
        1 => Ok(raw.to_vec()),
        5 => Ok(lzw::decode(raw, 8, Order::Msb, limit)),
        8 | 32946 => match zlib(raw, limit) {
            Ok(v) => Ok(v),
            Err(e) if !e.output.is_empty() => Ok(e.output),
            Err(_) => Err("TIFF strip data is corrupt".into()),
        },
        32773 => Ok(packbits(raw, limit)),
        other => Err(format!("TIFF compression {other} is not supported")),
    }
}

fn packbits(raw: &[u8], limit: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(limit.min(raw.len() * 2));
    let mut i = 0;
    while i < raw.len() && out.len() < limit {
        let n = raw[i] as i8;
        i += 1;
        if n >= 0 {
            let len = n as usize + 1;
            let end = (i + len).min(raw.len());
            out.extend_from_slice(&raw[i..end]);
            i = end;
        } else if n != -128 {
            let Some(&v) = raw.get(i) else { break };
            i += 1;
            out.extend(std::iter::repeat_n(v, (1 - i32::from(n)) as usize));
        }
    }
    out.truncate(limit);
    out
}

fn undo_predictor(data: &mut [u8], row_bytes: usize, samples: usize) {
    for line in data.chunks_mut(row_bytes.max(1)) {
        for i in samples..line.len() {
            line[i] = line[i].wrapping_add(line[i - samples]);
        }
    }
}

fn sample_8(line: &[u8], i: usize, bits: usize, big: bool) -> u8 {
    match bits {
        8 => line[i],
        // 16 位取高字节；字节序跟文件走
        16 => line[i * 2 + usize::from(!big)],
        4 => {
            let v = line[i / 2];
            (if i.is_multiple_of(2) { v >> 4 } else { v & 15 }) * 17
        }
        _ => {
            if (line[i / 8] >> (7 - i % 8)) & 1 == 1 {
                255
            } else {
                0
            }
        }
    }
}

fn gray_of(
    line: &[u8],
    x: usize,
    samples: usize,
    bits: usize,
    photometric: u32,
    map: Option<&[u32]>,
    big: bool,
) -> u8 {
    let s = |c: usize| sample_8(line, x * samples + c, bits, big);
    match (photometric, samples) {
        (0, _) => 255 - s(0),
        (3, _) => {
            // 调色板：下标是原始位值，表里 R、G、B 三段，每段 16 位
            let index = match bits {
                8 => usize::from(line[x]),
                4 => usize::from(s(0) / 17),
                _ => usize::from(s(0) == 255),
            };
            let Some(m) = map else { return s(0) };
            let n = m.len() / 3;
            if index >= n {
                return 255;
            }
            luma(
                (m[index] >> 8) as u8,
                (m[n + index] >> 8) as u8,
                (m[2 * n + index] >> 8) as u8,
            )
        }
        (5, 4..) => {
            let (r, g, b) = cmyk_to_rgb(s(0), s(1), s(2), s(3));
            luma(r, g, b)
        }
        (2, 4..) => over_white(luma(s(0), s(1), s(2)), s(3)),
        (2, 3) => luma(s(0), s(1), s(2)),
        (_, 2) => over_white(s(0), s(1)),
        _ => s(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个小端、单条带的 TIFF。标签按号排序，值都放得下 4 字节。
    fn tiff(tags: &[(u16, u32)], data: &[u8]) -> Vec<u8> {
        let offset = 8 + 2 + (tags.len() + 2) * 12 + 4;
        let mut all: Vec<(u16, u32)> = tags.to_vec();
        all.push((273, offset as u32));
        all.push((279, data.len() as u32));
        all.sort_by_key(|t| t.0);
        let mut b = b"II*\0".to_vec();
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&(all.len() as u16).to_le_bytes());
        for (id, v) in all {
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&4u16.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(data);
        b
    }

    fn px(tags: &[(u16, u32)], data: &[u8]) -> Vec<u8> {
        decode(&tiff(tags, data), 1_000).unwrap().pixels
    }

    #[test]
    fn plain_gray_rgb_and_polarity() {
        let g = [(256, 2), (257, 1), (258, 8), (262, 1)];
        assert_eq!(px(&g, &[10, 200]), [10, 200]);
        // WhiteIsZero
        let inv = [(256, 2), (257, 1), (258, 8), (262, 0)];
        assert_eq!(px(&inv, &[0, 255]), [255, 0]);
        let rgb = [(256, 1), (257, 1), (258, 8), (262, 2), (277, 3)];
        assert_eq!(px(&rgb, &[255, 255, 255]), [255]);
        let bits1 = [(256, 3), (257, 1), (258, 1), (262, 1)];
        assert_eq!(px(&bits1, &[0b1010_0000]), [255, 0, 255]);
    }

    #[test]
    fn packbits_lzw_deflate_and_predictor() {
        let base = [(256u16, 4u32), (257, 1), (258, 8), (262, 1)];
        let with = |c: u32| {
            let mut t = base.to_vec();
            t.push((259, c));
            t
        };
        // PackBits：2 个字面量，再重复 2 次
        assert_eq!(px(&with(32773), &[1, 9, 8, 0xFF, 7]), [9, 8, 7, 7]);
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&[1, 2, 3, 4], 1);
        assert_eq!(px(&with(8), &z), [1, 2, 3, 4]);
        assert_eq!(px(&with(32946), &z), [1, 2, 3, 4]);
        // TIFF 规范的 LZW 例子
        let lz = [
            0x80, 0x01, 0xE0, 0x40, 0x80, 0x44, 0x08, 0x0C, 0x06, 0x80, 0x80,
        ];
        assert_eq!(px(&with(5), &lz), [7, 7, 7, 8]);
        let mut pred = with(1);
        pred.push((317, 2));
        assert_eq!(px(&pred, &[10, 5, 5, 5]), [10, 15, 20, 25]);
    }

    #[test]
    fn unsupported_shapes_are_reported() {
        let t = |tags: &[(u16, u32)]| decode(&tiff(tags, &[0]), 100);
        assert!(
            t(&[(256, 1), (257, 1), (322, 16)])
                .unwrap_err()
                .contains("tiled")
        );
        assert!(
            t(&[(256, 1), (257, 1), (258, 8), (259, 99)])
                .unwrap_err()
                .contains("99")
        );
        assert!(t(&[(256, 1), (257, 1), (258, 12)]).is_err());
        assert!(t(&[(256, 1000), (257, 1000)]).is_err());
        assert!(decode(b"II*\0", 10).is_err());
        assert!(decode(b"II*\0\xff\xff\xff\xff", 10).is_err());
    }

    #[test]
    fn big_endian_header() {
        let mut b = b"MM\0*".to_vec();
        b.extend_from_slice(&8u32.to_be_bytes());
        b.extend_from_slice(&5u16.to_be_bytes());
        for (id, v) in [(256u16, 1u32), (257, 1), (258, 8), (273, 74), (279, 1)] {
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&4u16.to_be_bytes());
            b.extend_from_slice(&1u32.to_be_bytes());
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&[0; 4]);
        b.push(42);
        assert_eq!(decode(&b, 10).unwrap().pixels, [42]);
    }
}
