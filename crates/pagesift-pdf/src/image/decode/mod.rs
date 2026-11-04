//! 图像编码层的像素解码（给 OCR 用）：JPEG（ITU-T T.81）、CCITT 传真（T.4/T.6）、JBIG2（T.88）。导出（[`super::export`]）仍按原编码，不经过这里。

pub(crate) mod ccitt;
pub(crate) mod file;
pub(crate) mod jbig2;
pub(crate) mod jpeg;

/// 解码出的像素，按行存放、无行间填充。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Raster {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// 每像素的分量数：1（灰度）、3（RGB）或 4（CMYK，已按 Adobe 约定取正）。
    pub(crate) channels: u8,
    pub(crate) data: Vec<u8>,
}

impl Raster {
    /// 转成灰度（OCR 只需要亮度）。RGB 按 ITU-R BT.601 的亮度系数；CMYK 先换成 RGB。
    pub(crate) fn to_gray(&self) -> Raster {
        let data = match self.channels {
            // [协议] 分量数：1 是灰度，3 是 RGB，4 是 CMYK（ISO 32000-2 §8.6.4 的设备色彩空间）
            1 => self.data.clone(),
            3 => self
                .data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| luma(p[0], p[1], p[2]))
                .collect(),
            _ => self
                .data
                .chunks_exact(self.channels as usize)
                .map(|p| {
                    let (r, g, b) = cmyk_to_rgb(p[0], p[1], p[2], p[3]);
                    luma(r, g, b)
                })
                .collect(),
        };
        Raster {
            width: self.width,
            height: self.height,
            channels: 1,
            data,
        }
    }
}

/// [协议] 亮度系数 0.299/0.587/0.114（ITU-R BT.601；JFIF 1.02 用同一组）
pub(crate) fn luma(r: u8, g: u8, b: u8) -> u8 {
    let y = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b); // [协议] BT.601
    y.round().clamp(0.0, 255.0) as u8 // [协议] 8 位样本取值 0–255
}

/// 设备 CMYK 到 RGB 的朴素换算（ISO 32000-2 §10.4.2 的 DeviceCMYK → DeviceRGB）。
pub(crate) fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8) -> (u8, u8, u8) {
    let f = |x: u8| {
        let v = 255 - u16::from(x).saturating_add(u16::from(k)).min(255); // [协议] 8 位样本取值 0–255
        v as u8
    };
    (f(c), f(m), f(y))
}
