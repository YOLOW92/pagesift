//! JPEG 解码（ITU-T T.81）：顺序方式（SOF0/SOF1）和渐进方式（SOF2）的霍夫曼编码、8 位精度。
//! 算术编码（SOF9 起）、无损方式（SOF3）、分层方式和 12 位精度不支持，返回错误。

mod huffman;
mod idct;
mod marker;
mod scan;

use super::Raster;
use huffman::Huffman;
use marker::{BLOCK, COEFS, Frame};
use scan::Plane;

/// 颜色怎么解释：PDF 的 DCTDecode 参数 /ColorTransform 优先（ISO 32000-2 §7.4.8 表 13），
/// 其次 Adobe APP14 标记，最后按 JFIF 惯例（3 分量是 YCbCr）。
/// `max_bytes` 是解码占用内存的上限（防解压炸弹：几百字节的输入就能声明上亿像素），
/// PDF 里沿用 `Options::max_stream_bytes`。
pub(crate) fn decode(
    data: &[u8],
    color_transform: Option<bool>,
    max_bytes: usize,
) -> Result<Raster, String> {
    let p = parse(data, max_bytes)?;
    let samples = (0..p.frame.comps.len())
        .map(|k| p.samples(k))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(assemble(
        &p.frame,
        &samples,
        &p.planes,
        color_transform,
        p.adobe,
    ))
}

/// 直接解成灰度（OCR 用）。YCbCr 图取 Y：色度只做熵解码、不做 IDCT 和色彩变换；
/// 其余按 [`decode`] 解完再换灰度。
pub(crate) fn decode_gray(
    data: &[u8],
    color_transform: Option<bool>,
    max_bytes: usize,
) -> Result<Raster, String> {
    let p = parse(data, max_bytes)?;
    let n = p.frame.comps.len();
    let c0 = &p.frame.comps[0];
    // Y 分量本身就是全分辨率时才能直接拿它当灰度
    let full = c0.h == p.frame.hmax() && c0.v == p.frame.vmax();
    // [协议] 3 分量按 YCbCr 解释时第 0 个分量是 Y（JFIF / T.871）
    if n == 1 || (n == 3 && full && transform(n, color_transform, p.adobe)) {
        let y = p.samples(0)?;
        let only = Frame {
            comps: vec![p.frame.comps[0].clone()],
            ..p.frame.clone()
        };
        let r = assemble(&only, &[y], &p.planes[..1], Some(false), None);
        return Ok(r);
    }
    let samples = (0..n)
        .map(|k| p.samples(k))
        .collect::<Result<Vec<_>, String>>()?;
    Ok(assemble(&p.frame, &samples, &p.planes, color_transform, p.adobe).to_gray())
}

/// 熵解码完的系数和解释颜色需要的信息。
struct Parsed {
    frame: Frame,
    planes: Vec<Plane>,
    quant: [Option<[u16; COEFS]>; 4], // [协议] 量化表号 0–3（T.81 §B.2.4.1）
    adobe: Option<u8>,
}

impl Parsed {
    /// 第 k 个分量反量化 + IDCT 后的样本。
    fn samples(&self, k: usize) -> Result<Vec<u8>, String> {
        let tq = self.frame.comps[k].tq;
        let q = self.quant[tq]
            .as_ref()
            .ok_or_else(|| format!("量化表 {tq} 未定义"))?;
        Ok(inverse(&self.planes[k], q))
    }
}

/// [策略] 一张图的扫描次数上限：正常的渐进图十来次（libjpeg 缺省脚本彩色 10 次、灰度 6 次），
/// 手工脚本也到不了几百次；每次扫描都要走完所有块，而 10 字节就能写一个扫描，不设上限时小文件
/// 能占住工人很久。超过时整张图解码失败（libjpeg-turbo 的 -maxscans 同样如此）
const MAX_SCANS: usize = 1000;

/// 解这一帧大约要占的内存：各分量的系数（i16，块数按 MCU 补齐）加输出像素。
fn footprint(f: &Frame) -> u64 {
    let (mx, my) = f.mcus();
    let coefs: u64 = f
        .comps
        .iter()
        .map(|c| (mx * c.h * my * c.v * COEFS * size_of::<i16>()) as u64)
        .sum();
    coefs + (f.width * f.height * f.comps.len()) as u64
}

fn parse(data: &[u8], max_bytes: usize) -> Result<Parsed, String> {
    // [协议] SOI 标记 FFD8（T.81 §B.1.1.3）
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err("不以 SOI 标记开头".into());
    }
    let mut pos = 2;
    let mut frame: Option<Frame> = None;
    let mut planes: Vec<Plane> = Vec::new();
    let mut quant: [Option<[u16; COEFS]>; 4] = [None; 4]; // [协议] 量化表号 0–3（T.81 §B.2.4.1）
    let mut dc: [Option<Huffman>; 4] = Default::default(); // [协议] DC 表号 0–3（T.81 §B.2.4.2）
    let mut ac: [Option<Huffman>; 4] = Default::default(); // [协议] AC 表号 0–3（T.81 §B.2.4.2）
    let mut restart = 0usize;
    let mut adobe: Option<u8> = None;
    let mut scans = 0usize;
    while pos + 1 < data.len() {
        // [协议] 标记以 FF 开头（T.81 §B.1.1.2）
        if data[pos] != 0xFF {
            // 标记之间的垃圾字节：跳过（写入方的错误，解码器通常容忍）
            pos += 1;
            continue;
        }
        let m = data[pos + 1];
        pos += 2;
        match m {
            0xFF => pos -= 1,               // [协议] 填充字节 FF（T.81 §B.1.1.2）
            0xD8 | 0x01 | 0xD0..=0xD7 => {} // [协议] SOI、TEM、RSTm 没有段内容
            0xD9 => break,                  // [协议] EOI
            _ => {
                let (body, next) = marker::segment(data, pos)?;
                match m {
                    // [协议] SOF0 基线、SOF1 扩展顺序、SOF2 渐进（T.81 表 B.1）
                    0xC0..=0xC2 => {
                        if frame.is_some() {
                            return Err("出现了第二个帧头".into());
                        }
                        // [协议] SOF2 是渐进方式（T.81 表 B.1）
                        let f = marker::frame(body, m == 0xC2)?;
                        let need = footprint(&f);
                        if need > max_bytes as u64 {
                            return Err(format!("解码要 {need} 字节，超过上限 {max_bytes}"));
                        }
                        planes = (0..f.comps.len()).map(|c| Plane::new(&f, c)).collect();
                        frame = Some(f);
                    }
                    // [协议] 其余 SOFn：无损、分层、算术编码（DHT 是 C4、JPG 是 C8、DAC 是 CC，不在其中）
                    0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                        // [协议] SOFn 的标记是 FFC0 + n（T.81 表 B.1）
                        return Err(format!("不支持的编码方式 SOF{}", m - 0xC0));
                    }
                    0xC4 => marker::huffman(body, &mut dc, &mut ac)?, // [协议] DHT
                    0xDB => marker::quant(body, &mut quant)?,         // [协议] DQT
                    0xDD => {
                        // [协议] DRI：重启间隔 Ri，2 字节（T.81 §B.2.4.4）
                        let b = body.get(..2).ok_or("DRI 段太短")?;
                        restart = usize::from(u16::from_be_bytes([b[0], b[1]]));
                    }
                    0xEE => adobe = adobe.or(marker::adobe_transform(body)), // [协议] APP14
                    0xDA => {
                        // [协议] SOS
                        let f = frame.as_ref().ok_or("扫描出现在帧头之前")?;
                        if scans >= MAX_SCANS {
                            return Err(format!("扫描超过 {MAX_SCANS} 次"));
                        }
                        let s = marker::scan(body, f)?;
                        let t = scan::Tables {
                            dc: &dc,
                            ac: &ac,
                            restart,
                        };
                        pos = scan::decode(data, next, f, &s, &t, &mut planes)?;
                        scans += 1;
                        continue;
                    }
                    _ => {} // APPn、COM、DNL 等：跳过
                }
                pos = next;
            }
        }
    }
    let frame = frame.ok_or("没有帧头")?;
    if scans == 0 {
        return Err("没有扫描数据".into());
    }
    Ok(Parsed {
        frame,
        planes,
        quant,
        adobe,
    })
}

/// 一个分量反量化 + IDCT 后的样本，宽 `blocks_w·8`。
fn inverse(p: &Plane, q: &[u16; COEFS]) -> Vec<u8> {
    let idct = idct::Idct::new(q);
    let stride = p.blocks_w * BLOCK;
    let mut out = vec![0u8; stride * p.blocks_h * BLOCK];
    for row in 0..p.blocks_h {
        for col in 0..p.blocks_w {
            let i = (row * p.blocks_w + col) * COEFS;
            let at = row * BLOCK * stride + col * BLOCK;
            idct.block(&p.coefs[i..i + COEFS], &mut out[at..], stride);
        }
    }
    out
}

/// 默认：3 分量做 YCbCr 变换；4 分量只有 Adobe 标记说是 YCCK 才变换。
fn transform(n: usize, color_transform: Option<bool>, adobe: Option<u8>) -> bool {
    color_transform.unwrap_or(match (n, adobe) {
        // [协议] 3 分量按 JFIF 惯例是 YCbCr（T.871），Adobe 标志 0 表示不变换（Adobe 技术说明 #5116）
        (3, Some(t)) => t != 0,
        (3, None) => true,
        (4, Some(t)) => t == 2, // [协议] Adobe 变换 2 = YCCK
        _ => false,
    })
}

/// 各分量按采样因子放大到帧尺寸（最近邻复制），再换成灰度 / RGB / CMYK。
fn assemble(
    frame: &Frame,
    samples: &[Vec<u8>],
    planes: &[Plane],
    color_transform: Option<bool>,
    adobe: Option<u8>,
) -> Raster {
    let (w, h) = (frame.width, frame.height);
    let (hmax, vmax) = (frame.hmax(), frame.vmax());
    let n = frame.comps.len();
    if n == 1 {
        // 单分量：按行把有效宽度复制出来（样本行距按块补齐过）
        let stride = planes[0].blocks_w * BLOCK;
        let mut data = Vec::with_capacity(w * h);
        for y in 0..h {
            data.extend_from_slice(&samples[0][y * stride..y * stride + w]);
        }
        return Raster {
            width: w as u32,
            height: h as u32,
            channels: 1,
            data,
        };
    }
    // 每个分量在输出坐标上的取样位置（按列预先算好）
    let cols: Vec<Vec<usize>> = frame
        .comps
        .iter()
        .map(|c| (0..w).map(|x| x * c.h / hmax).collect())
        .collect();
    let at = |k: usize, x: usize, y: usize| -> u8 {
        let c = &frame.comps[k];
        let stride = planes[k].blocks_w * BLOCK;
        samples[k][(y * c.v / vmax) * stride + cols[k][x]]
    };
    let transform = transform(n, color_transform, adobe);
    // [协议] 3 分量是 RGB / YCbCr，4 分量是 CMYK / YCCK；2 分量没有约定的解释，只取第 0 个当灰度
    let channels = if n >= 3 { n } else { 1 };
    let mut data = Vec::with_capacity(w * h * channels);
    for y in 0..h {
        for x in 0..w {
            match n {
                // [协议] 3 分量 YCbCr（JFIF / T.871）；at 的第一个参数是分量号
                1 | 2 => data.push(at(0, x, y)),
                3 if transform => {
                    let (r, g, b) = ycc_to_rgb(at(0, x, y), at(1, x, y), at(2, x, y));
                    data.extend([r, g, b]);
                }
                // [协议] 3 分量不变换时就是 RGB，原样输出
                3 => data.extend([at(0, x, y), at(1, x, y), at(2, x, y)]),
                _ => {
                    let (c, m, yy, k) = if transform {
                        let (r, g, b) = ycc_to_rgb(at(0, x, y), at(1, x, y), at(2, x, y));
                        (255 - r, 255 - g, 255 - b, at(3, x, y)) // [协议] YCCK：YCC 换出 RGB 的补色即 CMY
                    } else {
                        // [协议] 4 分量不变换时就是 CMYK，分量号 0–3
                        (at(0, x, y), at(1, x, y), at(2, x, y), at(3, x, y))
                    };
                    // [协议] Adobe 写的 CMYK JPEG 存反相值 255 − v（Adobe 技术说明 #5116 的惯例）
                    let inv = |v: u8| if adobe.is_some() { 255 - v } else { v };
                    data.extend([inv(c), inv(m), inv(yy), inv(k)]);
                }
            }
        }
    }
    Raster {
        width: w as u32,
        height: h as u32,
        channels: channels as u8,
        data,
    }
}

/// [协议] JFIF 1.02 / ITU-T T.871 §7 的 YCbCr → RGB 系数
fn ycc_to_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    // [协议] 色差分量以 128 为零点（T.871 §7）
    let (y, cb, cr) = (f32::from(y), f32::from(cb) - 128.0, f32::from(cr) - 128.0);
    let clip = |v: f32| v.round().clamp(0.0, 255.0) as u8; // [协议] 8 位样本 0–255
    (
        clip(y + 1.402 * cr),
        clip(y - 0.344_136 * cb - 0.714_136 * cr),
        clip(y + 1.772 * cb),
    )
}

#[cfg(test)]
mod tests;
