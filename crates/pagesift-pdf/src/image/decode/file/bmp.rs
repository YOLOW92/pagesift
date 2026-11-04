//! Windows BMP：无压缩的 1、4、8、24、32 位，以及位域写法的标准 32 位。行从下往上（高度为负时从上往下），
//! 每行补齐到 4 字节。RLE 不支持，报明确错误。

use super::{GrayImage, check_size, luma};

pub(super) fn decode(b: &[u8], max_pixels: u64) -> Result<GrayImage, String> {
    let short = || "BMP is truncated".to_owned();
    let u16_at = |i: usize| -> Result<u16, String> {
        let s = b.get(i..i + 2).ok_or_else(short)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    };
    let u32_at = |i: usize| -> Result<u32, String> {
        let s = b.get(i..i + 4).ok_or_else(short)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let offset = u32_at(10)? as usize;
    let dib = u32_at(14)? as usize;
    if dib < 40 {
        return Err("BMP core headers are not supported".into());
    }
    let width = u32_at(18)?;
    let raw_height = u32_at(22)? as i32;
    let bits = usize::from(u16_at(28)?);
    let compression = u32_at(30)?;
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs();
    let total = check_size(width, height, max_pixels)?;
    if !(compression == 0 || (compression == 3 && bits == 32)) {
        return Err(format!("BMP compression {compression} is not supported"));
    }
    if !matches!(bits, 1 | 4 | 8 | 24 | 32) {
        return Err(format!("BMP with {bits} bits per pixel is not supported"));
    }
    let palette: Vec<u8> = if bits <= 8 {
        let used = u32_at(46)? as usize;
        let n = if used == 0 {
            1usize << bits
        } else {
            used.min(1 << bits)
        };
        let table = b.get(14 + dib..14 + dib + n * 4).ok_or_else(short)?;
        table
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| luma(c[2], c[1], c[0]))
            .collect()
    } else {
        Vec::new()
    };
    let (w, h) = (width as usize, height as usize);
    let stride = (w * bits).div_ceil(32) * 4;
    let mut pixels = vec![255u8; total];
    let data = b.get(offset..).ok_or_else(short)?;
    for row in 0..h {
        let Some(line) = data.get(row * stride..(row + 1) * stride) else {
            break; // 截断：其余留白
        };
        let y = if top_down { row } else { h - 1 - row };
        let out = &mut pixels[y * w..(y + 1) * w];
        for (x, p) in out.iter_mut().enumerate() {
            *p = match bits {
                24 => luma(line[x * 3 + 2], line[x * 3 + 1], line[x * 3]),
                32 => luma(line[x * 4 + 2], line[x * 4 + 1], line[x * 4]),
                8 => palette.get(usize::from(line[x])).copied().unwrap_or(255),
                4 => {
                    let v = line[x / 2];
                    let i = if x % 2 == 0 { v >> 4 } else { v & 15 };
                    palette.get(usize::from(i)).copied().unwrap_or(255)
                }
                _ => {
                    let i = (line[x / 8] >> (7 - x % 8)) & 1;
                    palette.get(usize::from(i)).copied().unwrap_or(255)
                }
            };
        }
    }
    Ok(GrayImage {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bmp(w: i32, h: i32, bits: u16, compression: u32, palette: &[u8], data: &[u8]) -> Vec<u8> {
        let offset = 54 + palette.len() as u32;
        let mut b = b"BM".to_vec();
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&offset.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(&compression.to_le_bytes());
        b.extend_from_slice(&[0; 20]);
        b.extend_from_slice(palette);
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn bottom_up_and_top_down_rows() {
        let gray8: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v, 0]).collect();
        let data = [10, 20, 0, 0, 30, 40, 0, 0];
        let up = decode(&bmp(2, 2, 8, 0, &gray8, &data), 100).unwrap();
        assert_eq!(up.pixels, [30, 40, 10, 20]);
        let down = decode(&bmp(2, -2, 8, 0, &gray8, &data), 100).unwrap();
        assert_eq!(down.pixels, [10, 20, 30, 40]);
    }

    #[test]
    fn true_color_and_low_bit_depths() {
        assert_eq!(
            decode(&bmp(1, 1, 24, 0, &[], &[0, 0, 0, 0]), 10)
                .unwrap()
                .pixels,
            [0]
        );
        assert_eq!(
            decode(&bmp(1, 1, 32, 3, &[], &[255, 255, 255, 0]), 10)
                .unwrap()
                .pixels,
            [255]
        );
        let bw: Vec<u8> = vec![0, 0, 0, 0, 255, 255, 255, 0];
        assert_eq!(
            decode(&bmp(3, 1, 1, 0, &bw, &[0b1010_0000, 0, 0, 0]), 10)
                .unwrap()
                .pixels,
            [255, 0, 255]
        );
        let mut pal4 = vec![0u8; 16 * 4];
        pal4[4..8].copy_from_slice(&[255, 255, 255, 0]);
        assert_eq!(
            decode(&bmp(2, 1, 4, 0, &pal4, &[0x01, 0, 0, 0]), 10)
                .unwrap()
                .pixels,
            [0, 255]
        );
    }

    #[test]
    fn unsupported_and_truncated() {
        assert!(
            decode(&bmp(1, 1, 8, 1, &[], &[]), 10)
                .unwrap_err()
                .contains("compression")
        );
        assert!(decode(&bmp(1, 1, 16, 0, &[], &[]), 10).is_err());
        assert!(decode(&bmp(100, 100, 24, 0, &[], &[]), 10).is_err());
        assert!(decode(b"BM", 10).is_err());
        // 数据不够：其余留白
        assert_eq!(
            decode(&bmp(1, 2, 24, 0, &[], &[0, 0, 0, 0]), 10)
                .unwrap()
                .pixels,
            [255, 0]
        );
    }
}
