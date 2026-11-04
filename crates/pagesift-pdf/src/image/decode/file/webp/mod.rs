//! WebP 文件：RIFF 容器里是 VP8（有损）或 VP8L（无损）。动画只取第一帧的情况不处理，报明确错误。

mod tables;
mod vp8;
mod vp8l;

use super::{GrayImage, check_size, over_white};
use crate::image::decode::luma;

pub(super) fn decode(bytes: &[u8], max_pixels: u64) -> Result<GrayImage, String> {
    let check = |w: u64, h: u64| check_size(w as u32, h as u32, max_pixels).map(|_| ());
    let mut p = 12usize;
    while let Some(head) = bytes.get(p..p + 8) {
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let end = (p + 8).saturating_add(size).min(bytes.len());
        let body = &bytes[p + 8..end];
        match &head[..4] {
            b"VP8 " => {
                let (mut pixels, width, height) = vp8::decode_luma(body, &check)?;
                // VP8 的亮度是 16–235 的有限范围，转成全范围灰度
                for v in &mut pixels {
                    *v = (((i32::from(*v) - 16) * 298 + 128) >> 8).clamp(0, 255) as u8;
                }
                return Ok(GrayImage {
                    width: width as u32,
                    height: height as u32,
                    pixels,
                });
            }
            b"VP8L" => {
                let (argb, width, height) = vp8l::decode(body, &check)?;
                let pixels = argb
                    .iter()
                    .map(|&p| {
                        let v = luma((p >> 16) as u8, (p >> 8) as u8, p as u8);
                        over_white(v, (p >> 24) as u8)
                    })
                    .collect();
                return Ok(GrayImage {
                    width: width as u32,
                    height: height as u32,
                    pixels,
                });
            }
            b"ANIM" | b"ANMF" => return Err("animated WebP images are not supported".into()),
            _ => {}
        }
        // 块按偶数字节对齐
        p = (p + 8).saturating_add(size).saturating_add(size & 1);
    }
    Err("WebP: no image data".into())
}
