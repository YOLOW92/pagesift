//! JPEG 标记段的解析（ITU-T T.81 附录 B）：帧头、扫描头、量化表、霍夫曼表、重启间隔，
//! 以及 Adobe APP14 里的色彩变换标志。

use super::huffman::Huffman;

/// 帧里的一个分量（T.81 §B.2.2 表 B.2）。
#[derive(Debug, Clone)]
pub(super) struct Component {
    pub(super) id: u8,
    /// 水平、垂直采样因子（1–4）。
    pub(super) h: usize,
    pub(super) v: usize,
    /// 量化表号（0–3）。
    pub(super) tq: usize,
}

/// 帧头（SOFn）。
#[derive(Debug, Clone)]
pub(super) struct Frame {
    pub(super) progressive: bool,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) comps: Vec<Component>,
}

impl Frame {
    pub(super) fn hmax(&self) -> usize {
        self.comps.iter().map(|c| c.h).max().unwrap_or(1)
    }
    pub(super) fn vmax(&self) -> usize {
        self.comps.iter().map(|c| c.v).max().unwrap_or(1)
    }
    /// 一个 MCU 覆盖 8·Hmax × 8·Vmax 个像素；按 MCU 计的列数、行数（T.81 §A.2.4）。
    pub(super) fn mcus(&self) -> (usize, usize) {
        (
            self.width.div_ceil(BLOCK * self.hmax()),
            self.height.div_ceil(BLOCK * self.vmax()),
        )
    }
}

/// [协议] DCT 块边长 8（T.81 §A.3.3）
pub(super) const BLOCK: usize = 8;
/// [协议] 一块 64 个系数
pub(super) const COEFS: usize = BLOCK * BLOCK;

/// 扫描头（SOS，T.81 §B.2.3）里的一个分量：在帧分量里的下标、DC 表号、AC 表号。
#[derive(Debug, Clone, Copy)]
pub(super) struct ScanComp {
    pub(super) index: usize,
    pub(super) td: usize,
    pub(super) ta: usize,
}

#[derive(Debug, Clone)]
pub(super) struct Scan {
    pub(super) comps: Vec<ScanComp>,
    /// 频谱选择的起止（Ss、Se）与逐次逼近位（Ah、Al），T.81 §G.1.1.1。
    pub(super) ss: usize,
    pub(super) se: usize,
    pub(super) ah: u32,
    pub(super) al: u32,
}

/// 读一个带长度的标记段，返回段内容（不含两字节长度）和段之后的位置。
pub(super) fn segment(data: &[u8], pos: usize) -> Result<(&[u8], usize), String> {
    let len = data
        .get(pos..pos + 2)
        .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        .ok_or("标记段长度被截断")?;
    if len < 2 {
        return Err("标记段长度小于 2".into());
    }
    let body = data.get(pos + 2..pos + len).ok_or("标记段被截断")?;
    Ok((body, pos + len))
}

/// SOFn（T.81 §B.2.2）。只支持 8 位精度。
pub(super) fn frame(body: &[u8], progressive: bool) -> Result<Frame, String> {
    // [协议] 帧头：P(1) Y(2) X(2) Nf(1)，每分量 3 字节（T.81 表 B.2）
    if body.len() < 6 {
        return Err("帧头太短".into());
    }
    // [协议] 样本精度 P 只支持 8 位；12 位（扩展方式）不支持（T.81 表 B.2）
    if body[0] != 8 {
        return Err(format!("不支持 {} 位样本精度", body[0]));
    }
    let height = usize::from(u16::from_be_bytes([body[1], body[2]]));
    let width = usize::from(u16::from_be_bytes([body[3], body[4]]));
    let n = usize::from(body[5]);
    if width == 0 || height == 0 {
        // 高度为 0 时由 DNL 段给出（T.81 §B.2.5），写入方几乎不用，不支持
        return Err("帧宽或帧高为 0".into());
    }
    // [协议] 6 字节固定头 + 每分量 3 字节（T.81 表 B.2）
    if body.len() < 6 + 3 * n || n == 0 {
        return Err("帧头分量表被截断".into());
    }
    // [协议] PDF 的 DCTDecode 只有 1、3、4 个分量（ISO 32000-2 §7.4.8）；2 个分量只取第 0 个当灰度。
    // 再多没有约定的解释，输出的像素也按不上 w×h
    if n > 4 {
        return Err(format!("不支持 {n} 个分量"));
    }
    let comps = (0..n)
        .map(|i| {
            // [协议] 第 i 个分量的 3 字节 Ci、Hi/Vi、Tqi，排在 6 字节固定头之后（T.81 表 B.2）
            let b = &body[6 + 3 * i..9 + 3 * i];
            let (h, v) = (usize::from(b[1] >> 4), usize::from(b[1] & 0x0F)); // [协议] 采样因子各占 4 位
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                // [协议] 采样因子取值 1–4（T.81 表 B.2）
                return Err(format!("分量 {} 的采样因子 {h}×{v} 不合法", b[0]));
            }
            let tq = usize::from(b[2]);
            // [协议] 量化表号 0–3（T.81 表 B.2）
            if tq > 3 {
                return Err(format!("量化表号 {tq} 不合法"));
            }
            Ok(Component { id: b[0], h, v, tq })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Frame {
        progressive,
        width,
        height,
        comps,
    })
}

/// DQT（T.81 §B.2.4.1）：可以一次定义多张表；表按之字形顺序存放，这里换成自然顺序。
/// [协议] 表号 0–3，共 4 张（T.81 §B.2.4.1）
pub(super) fn quant(body: &[u8], tables: &mut [Option<[u16; COEFS]>; 4]) -> Result<(), String> {
    let mut i = 0;
    while i < body.len() {
        let (pq, tq) = (body[i] >> 4, usize::from(body[i] & 0x0F)); // [协议] Pq、Tq 各占 4 位
        if tq > 3 {
            // [协议] 量化表号 0–3
            return Err(format!("量化表号 {tq} 不合法"));
        }
        let wide = pq != 0; // Pq = 1 时每项 16 位
        let size = if wide { 2 * COEFS } else { COEFS };
        let raw = body.get(i + 1..i + 1 + size).ok_or("量化表被截断")?;
        let mut t = [0u16; COEFS];
        for (k, &z) in ZIGZAG.iter().enumerate() {
            t[z] = if wide {
                u16::from_be_bytes([raw[2 * k], raw[2 * k + 1]])
            } else {
                u16::from(raw[k])
            };
        }
        tables[tq] = Some(t);
        i += 1 + size;
    }
    Ok(())
}

/// DHT（T.81 §B.2.4.2）：可以一次定义多张表。
/// [协议] `dc`、`ac` 各 4 张，表号 0–3（T.81 §B.2.4.2）
pub(super) fn huffman(
    body: &[u8],
    dc: &mut [Option<Huffman>; 4],
    ac: &mut [Option<Huffman>; 4],
) -> Result<(), String> {
    // [协议] 每张表：Tc/Th(1) + 16 个码长计数 + 码值（T.81 表 B.5）
    const LENGTHS: usize = 16;
    let mut i = 0;
    while i < body.len() {
        let (tc, th) = (body[i] >> 4, usize::from(body[i] & 0x0F)); // [协议] Tc、Th 各占 4 位
        if th > 3 || tc > 1 {
            // [协议] 表号 0–3，表类 0（DC）或 1（AC）
            return Err(format!("霍夫曼表头 {:#04x} 不合法", body[i]));
        }
        let counts = body.get(i + 1..i + 1 + LENGTHS).ok_or("霍夫曼表被截断")?;
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        let values = body
            .get(i + 1 + LENGTHS..i + 1 + LENGTHS + total)
            .ok_or("霍夫曼表被截断")?;
        let table = Huffman::new(counts, values)?;
        if tc == 0 {
            dc[th] = Some(table);
        } else {
            ac[th] = Some(table);
        }
        i += 1 + LENGTHS + total;
    }
    Ok(())
}

/// SOS（T.81 §B.2.3）。
pub(super) fn scan(body: &[u8], frame: &Frame) -> Result<Scan, String> {
    let n = usize::from(*body.first().ok_or("扫描头为空")?);
    // [协议] Ns(1)，每分量 2 字节，再加 Ss、Se、Ah/Al 共 3 字节（T.81 表 B.3）
    if n == 0 || n > 4 || body.len() < 1 + 2 * n + 3 {
        return Err("扫描头不合法".into());
    }
    let comps = (0..n)
        .map(|k| {
            let (id, t) = (body[1 + 2 * k], body[2 + 2 * k]);
            let index = frame
                .comps
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| format!("扫描引用了帧里没有的分量 {id}"))?;
            Ok(ScanComp {
                index,
                td: usize::from(t >> 4),   // [协议] Td、Ta 各占 4 位
                ta: usize::from(t & 0x0F), // [协议] Td、Ta 各占 4 位
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let tail = &body[1 + 2 * n..];
    let (ss, se) = (usize::from(tail[0]), usize::from(tail[1]));
    let (ah, al) = (u32::from(tail[2] >> 4), u32::from(tail[2] & 0x0F)); // [协议] Ah、Al 各占 4 位
    if se >= COEFS || ss > se {
        return Err(format!("频谱选择 {ss}..{se} 不合法"));
    }
    Ok(Scan {
        comps,
        ss,
        se,
        ah,
        al,
    })
}

/// Adobe APP14（Adobe 技术说明 #5116）：返回色彩变换标志（0 不变换，1 YCbCr，2 YCCK）。
pub(super) fn adobe_transform(body: &[u8]) -> Option<u8> {
    // [协议] "Adobe" 标识 + 版本(2) + 标志(2+2) + 变换(1) 共 12 字节
    const LEN: usize = 12;
    (body.len() >= LEN && body.starts_with(b"Adobe")).then(|| body[LEN - 1])
}

/// 之字形序号 → 自然顺序下标（T.81 图 A.6）。
#[rustfmt::skip]
pub(super) const ZIGZAG: [usize; COEFS] = [ // [协议] T.81 图 A.6 之字形顺序
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];
