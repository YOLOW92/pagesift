//! PNG（ISO/IEC 15948）：全部色型和位深，含调色板透明和 Adam7 隔行。解到灰度，透明按白底合成。

use super::{GrayImage, check_size, luma, over_white};

/// Adam7 的七趟：起点 x、起点 y、x 步长、y 步长。
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

struct Header {
    width: usize,
    height: usize,
    depth: usize,
    color: u8,
    interlaced: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }
}

pub(super) fn decode(bytes: &[u8], max_pixels: u64) -> Result<GrayImage, String> {
    let mut pos = 8usize;
    let mut header = None;
    let mut palette: &[u8] = &[];
    let mut trns: &[u8] = &[];
    let mut idat: Vec<u8> = Vec::new();
    while let Some(len) = bytes.get(pos..pos + 4) {
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let Some(kind) = bytes.get(pos + 4..pos + 8) else {
            break;
        };
        // 截断的文件：有多少用多少
        let body_end = (pos + 8).saturating_add(len).min(bytes.len());
        let body = &bytes[pos + 8..body_end];
        match kind {
            b"IHDR" => header = Some(parse_header(body, max_pixels)?),
            b"PLTE" => palette = body,
            b"tRNS" => trns = body,
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = body_end.saturating_add(4);
    }
    let h = header.ok_or("PNG has no IHDR")?;
    if h.color == 3 && palette.is_empty() {
        return Err("PNG palette is missing".into());
    }
    let bits = h.channels() * h.depth;
    // 解压上限：每行 1 字节过滤类型加像素，隔行时七趟总量不超过两倍
    let raw_size = (h.width * bits).div_ceil(8).saturating_add(1) * h.height;
    let limit = if h.interlaced {
        raw_size.saturating_mul(2).saturating_add(64)
    } else {
        raw_size
    };
    let raw = inflate(&idat, limit)?;
    let mut pixels = vec![255u8; h.width * h.height];
    let passes: &[(usize, usize, usize, usize)] = if h.interlaced {
        &ADAM7
    } else {
        &[(0, 0, 1, 1)]
    };
    let mut at = 0usize;
    let bytes_per_pixel = bits.div_ceil(8).max(1);
    for &(x0, y0, dx, dy) in passes {
        let pass_w = (h.width + dx - 1 - x0.min(h.width)) / dx;
        let pass_h = (h.height + dy - 1 - y0.min(h.height)) / dy;
        if pass_w == 0 || pass_h == 0 {
            continue;
        }
        let row_bytes = (pass_w * bits).div_ceil(8);
        let mut prev = vec![0u8; row_bytes];
        let mut cur = vec![0u8; row_bytes];
        for row in 0..pass_h {
            // 数据不够时剩下的留白：受损的图片能读多少读多少
            let Some(filter) = raw.get(at).copied() else {
                return finish(h, pixels);
            };
            let end = (at + 1 + row_bytes).min(raw.len());
            let src = &raw[at + 1..end];
            cur[..src.len()].copy_from_slice(src);
            cur[src.len()..].fill(0);
            unfilter(filter, &mut cur, &prev, bytes_per_pixel)?;
            at = end;
            for col in 0..pass_w {
                let y = y0 + row * dy;
                let x = x0 + col * dx;
                pixels[y * h.width + x] = gray_at(&h, &cur, col, palette, trns);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
    }
    finish(h, pixels)
}

fn finish(h: Header, pixels: Vec<u8>) -> Result<GrayImage, String> {
    Ok(GrayImage {
        width: h.width as u32,
        height: h.height as u32,
        pixels,
    })
}

fn parse_header(b: &[u8], max_pixels: u64) -> Result<Header, String> {
    if b.len() < 13 {
        return Err("PNG IHDR is too short".into());
    }
    let width = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let height = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    check_size(width, height, max_pixels)?;
    let (depth, color) = (usize::from(b[8]), b[9]);
    let valid = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !valid {
        return Err(format!("PNG color type {color} depth {depth} is invalid"));
    }
    Ok(Header {
        width: width as usize,
        height: height as usize,
        depth,
        color,
        interlaced: b[12] == 1,
    })
}

fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_zlib_with_limit};
    match decompress_to_vec_zlib_with_limit(data, limit) {
        Ok(v) => Ok(v),
        // 多出来的字节不影响前面的像素
        Err(e) if matches!(e.status, TINFLStatus::HasMoreOutput) => Ok(e.output),
        // 截断或校验不对时保留已解出的部分
        Err(e) if !e.output.is_empty() => Ok(e.output),
        Err(_) => Err("PNG image data is corrupt".into()),
    }
}

fn unfilter(kind: u8, cur: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), String> {
    match kind {
        0 => {}
        1 => {
            for i in bpp..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        2 => {
            for (c, p) in cur.iter_mut().zip(prev) {
                *c = c.wrapping_add(*p);
            }
        }
        3 => {
            for i in 0..cur.len() {
                let left = if i >= bpp { u16::from(cur[i - bpp]) } else { 0 };
                cur[i] = cur[i].wrapping_add(((left + u16::from(prev[i])) / 2) as u8);
            }
        }
        4 => {
            for i in 0..cur.len() {
                let a = if i >= bpp { cur[i - bpp] } else { 0 };
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                cur[i] = cur[i].wrapping_add(paeth(a, prev[i], c));
            }
        }
        k => return Err(format!("PNG filter {k} is invalid")),
    }
    Ok(())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i32::from(a), i32::from(b), i32::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// 第 `i` 个样本，缩放到 8 位（调色板下标不缩放）。16 位取高字节。
fn sample(row: &[u8], depth: usize, i: usize, scale: bool) -> u8 {
    match depth {
        8 => row[i],
        16 => row[i * 2],
        d => {
            let bit = i * d;
            let shift = 8 - d - bit % 8;
            let v = (row[bit / 8] >> shift) & ((1u8 << d) - 1);
            if scale {
                (u32::from(v) * 255 / ((1u32 << d) - 1)) as u8
            } else {
                v
            }
        }
    }
}

fn gray_at(h: &Header, row: &[u8], x: usize, palette: &[u8], trns: &[u8]) -> u8 {
    let d = h.depth;
    match h.color {
        0 => sample(row, d, x, true),
        2 => luma(
            sample(row, d, x * 3, true),
            sample(row, d, x * 3 + 1, true),
            sample(row, d, x * 3 + 2, true),
        ),
        3 => {
            let i = usize::from(sample(row, d, x, false));
            let Some(c) = palette.get(i * 3..i * 3 + 3) else {
                return 255;
            };
            let alpha = trns.get(i).copied().unwrap_or(255);
            over_white(luma(c[0], c[1], c[2]), alpha)
        }
        4 => over_white(sample(row, d, x * 2, true), sample(row, d, x * 2 + 1, true)),
        _ => over_white(
            luma(
                sample(row, d, x * 4, true),
                sample(row, d, x * 4 + 1, true),
                sample(row, d, x * 4 + 2, true),
            ),
            sample(row, d, x * 4 + 3, true),
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0; 4]); // 不校验 CRC：解码器不看它
    }

    pub(crate) fn png(
        w: u32,
        h: u32,
        depth: u8,
        color: u8,
        interlace: u8,
        raw: &[u8],
        extra: &[(&[u8], &[u8])],
    ) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, color, 0, 0, interlace]);
        chunk(&mut out, b"IHDR", &ihdr);
        for (k, v) in extra {
            chunk(&mut out, k, v);
        }
        chunk(
            &mut out,
            b"IDAT",
            &miniz_oxide::deflate::compress_to_vec_zlib(raw, 1),
        );
        chunk(&mut out, b"IEND", b"");
        out
    }

    fn px(p: Vec<u8>) -> Vec<u8> {
        decode(&p, 1_000).unwrap().pixels
    }

    #[test]
    fn gray_rgb_alpha_and_low_depths() {
        assert_eq!(px(png(2, 1, 8, 0, 0, &[0, 10, 200], &[])), [10, 200]);
        assert_eq!(px(png(2, 1, 16, 0, 0, &[0, 10, 9, 200, 9], &[])), [10, 200]);
        assert_eq!(
            px(png(4, 1, 1, 0, 0, &[0, 0b1010_0000], &[])),
            [255, 0, 255, 0]
        );
        assert_eq!(px(png(2, 1, 2, 0, 0, &[0, 0b1101_0000], &[])), [255, 85]);
        assert_eq!(px(png(1, 1, 8, 2, 0, &[0, 255, 255, 255], &[])), [255]);
        // 全透明的黑是白底
        assert_eq!(px(png(1, 1, 8, 4, 0, &[0, 0, 0], &[])), [255]);
        assert_eq!(px(png(1, 1, 8, 6, 0, &[0, 0, 0, 0, 255], &[])), [0]);
    }

    #[test]
    fn palette_with_transparency() {
        let plte: &[u8] = &[0, 0, 0, 255, 255, 255];
        let extra: [(&[u8], &[u8]); 2] = [(b"PLTE", plte), (b"tRNS", &[0])];
        assert_eq!(px(png(2, 1, 8, 3, 0, &[0, 0, 1], &extra)), [255, 255]);
        assert_eq!(px(png(1, 1, 8, 3, 0, &[0, 0], &[(b"PLTE", plte)])), [0]);
        assert!(decode(&png(1, 1, 8, 3, 0, &[0, 0], &[]), 10).is_err());
    }

    #[test]
    fn every_filter_type() {
        // 两行两像素，第二行分别用 1–4 号过滤器还原出同样的结果
        let first = [0u8, 10, 20];
        for (kind, row) in [
            (1u8, [30u8, 10]), // Sub：30、40
            (2, [20, 20]),     // Up：30、40
            (3, [25, 15]),     // Average：25+5=30、15+(30+20)/2=40
            (4, [20, 10]),     // Paeth：20+10=30、10+30=40
        ] {
            let mut raw = first.to_vec();
            raw.push(kind);
            raw.extend_from_slice(&row);
            assert_eq!(
                px(png(2, 2, 8, 0, 0, &raw, &[])),
                [10, 20, 30, 40],
                "filter {kind}"
            );
        }
        assert!(decode(&png(1, 1, 8, 0, 0, &[9, 0], &[]), 10).is_err());
    }

    #[test]
    fn adam7_interlace_places_pixels() {
        // 3×1：第 1 趟 x=0；第 4 趟 x=2；第 6 趟 x=1
        let raw = [0, 11, 0, 33, 0, 22];
        assert_eq!(px(png(3, 1, 8, 0, 1, &raw, &[])), [11, 22, 33]);
    }

    #[test]
    fn limits_and_damage() {
        assert!(decode(&png(100, 100, 8, 0, 0, &[0], &[]), 100).is_err());
        // 数据不够：有多少画多少，其余留白
        assert_eq!(px(png(2, 2, 8, 0, 0, &[0, 1, 2], &[])), [1, 2, 255, 255]);
        assert!(decode(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], 10).is_err());
    }
}
