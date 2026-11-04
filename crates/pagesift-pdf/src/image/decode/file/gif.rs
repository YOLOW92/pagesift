//! GIF87a/89a 的第一帧，贴到白底画布上。透明色和隔行都按规范处理。

use super::lzw::{self, Order};
use super::{GrayImage, check_size, luma};

pub(super) fn decode(b: &[u8], max_pixels: u64) -> Result<GrayImage, String> {
    let short = || "GIF is truncated".to_owned();
    let u16_at = |i: usize| -> Result<usize, String> {
        let s = b.get(i..i + 2).ok_or_else(short)?;
        Ok(usize::from(u16::from_le_bytes([s[0], s[1]])))
    };
    let (cw, ch) = (u16_at(6)?, u16_at(8)?);
    let total = check_size(cw as u32, ch as u32, max_pixels)?;
    let flags = *b.get(10).ok_or_else(short)?;
    let mut pos = 13usize;
    let mut global: &[u8] = &[];
    if flags & 0x80 != 0 {
        let n = 3usize << ((flags & 7) + 1);
        global = b.get(pos..pos + n).ok_or_else(short)?;
        pos += n;
    }
    let mut transparent = None;
    loop {
        match *b.get(pos).ok_or_else(short)? {
            0x21 => {
                let label = *b.get(pos + 1).ok_or_else(short)?;
                pos += 2;
                // 图形控制扩展：第 2 个字节的最低位表示透明色有效
                if label == 0xF9 {
                    let packed = *b.get(pos + 1).ok_or_else(short)?;
                    let index = *b.get(pos + 4).ok_or_else(short)?;
                    transparent = (packed & 1 == 1).then_some(index);
                }
                pos = skip_blocks(b, pos)?;
            }
            0x2C => break,
            0x3B => return Err("GIF has no image".into()),
            other => return Err(format!("GIF block {other:#x} is invalid")),
        }
    }
    let (left, top, w, h) = (
        u16_at(pos + 1)?,
        u16_at(pos + 3)?,
        u16_at(pos + 5)?,
        u16_at(pos + 7)?,
    );
    let local_flags = *b.get(pos + 9).ok_or_else(short)?;
    pos += 10;
    let mut table = global;
    if local_flags & 0x80 != 0 {
        let n = 3usize << ((local_flags & 7) + 1);
        table = b.get(pos..pos + n).ok_or_else(short)?;
        pos += n;
    }
    if table.is_empty() {
        return Err("GIF has no color table".into());
    }
    let min_width = u32::from(*b.get(pos).ok_or_else(short)?);
    if !(2..=11).contains(&min_width) {
        return Err("GIF code size is invalid".into());
    }
    pos += 1;
    let mut packed = Vec::new();
    while let Some(&n) = b.get(pos) {
        if n == 0 {
            break;
        }
        let Some(block) = b.get(pos + 1..pos + 1 + usize::from(n)) else {
            // 截断：有多少用多少
            packed.extend_from_slice(&b[pos + 1..]);
            break;
        };
        packed.extend_from_slice(block);
        pos += 1 + usize::from(n);
    }
    let frame = lzw::decode(&packed, min_width, Order::Lsb, w.saturating_mul(h));
    let mut pixels = vec![255u8; total];
    let interlaced = local_flags & 0x40 != 0;
    let rows = row_order(h, interlaced);
    for (i, &index) in frame.iter().enumerate() {
        if w == 0 {
            break;
        }
        if Some(index) == transparent {
            continue;
        }
        let (sy, x) = (i / w, i % w);
        let (Some(&y), Some(c)) = (
            rows.get(sy),
            table.get(usize::from(index) * 3..usize::from(index) * 3 + 3),
        ) else {
            continue;
        };
        let (cx, cy) = (left + x, top + y);
        if cx < cw && cy < ch {
            pixels[cy * cw + cx] = luma(c[0], c[1], c[2]);
        }
    }
    Ok(GrayImage {
        width: cw as u32,
        height: ch as u32,
        pixels,
    })
}

/// 第 i 个存储行在图上的行号。隔行：每 8 行从 0、每 8 行从 4、每 4 行从 2、每 2 行从 1。
fn row_order(h: usize, interlaced: bool) -> Vec<usize> {
    if !interlaced {
        return (0..h).collect();
    }
    [(0, 8), (4, 8), (2, 4), (1, 2)]
        .into_iter()
        .flat_map(|(start, step)| (start..h).step_by(step))
        .collect()
}

fn skip_blocks(b: &[u8], mut pos: usize) -> Result<usize, String> {
    loop {
        let n = usize::from(*b.get(pos).ok_or("GIF is truncated")?);
        pos += 1 + n;
        if n == 0 {
            return Ok(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GIF 规范的 10×10 示例，改用黑（0）和白（1）。
    fn sample(extra: &[u8], lflags: u8) -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&[10, 0, 10, 0, 0x91, 0, 0]);
        g.extend_from_slice(&[255, 255, 255, 0, 0, 0, 90, 90, 90, 200, 200, 200]);
        g.extend_from_slice(extra);
        g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 10, 0, 10, 0, lflags, 2, 22]);
        g.extend_from_slice(&[
            0x8C, 0x2D, 0x99, 0x87, 0x2A, 0x1C, 0xDC, 0x33, 0xA0, 0x02, 0x75, 0xEC, 0x95, 0xFA,
            0xA8, 0xDE, 0x60, 0x8C, 0x04, 0x91, 0x4C, 0x01,
        ]);
        g.extend_from_slice(&[0, 0x3B]);
        g
    }

    #[test]
    fn first_frame_with_palette() {
        let img = decode(&sample(&[], 0), 1_000).unwrap();
        assert_eq!((img.width, img.height), (10, 10));
        // 第一行是 5 个下标 1（黑）和 5 个下标 2（灰 90）
        assert_eq!(&img.pixels[..6], &[0, 0, 0, 0, 0, 90]);
    }

    #[test]
    fn transparency_shows_white() {
        let gce = [0x21, 0xF9, 4, 1, 0, 0, 2, 0];
        let img = decode(&sample(&gce, 0), 1_000).unwrap();
        assert_eq!(&img.pixels[4..6], &[0, 255]);
    }

    #[test]
    fn interlace_order_and_limits() {
        assert_eq!(row_order(5, true), [0, 4, 2, 1, 3]);
        assert_eq!(row_order(3, false), [0, 1, 2]);
        assert!(decode(&sample(&[], 0), 10).is_err());
        assert!(decode(b"GIF89a", 100).is_err());
    }
}
