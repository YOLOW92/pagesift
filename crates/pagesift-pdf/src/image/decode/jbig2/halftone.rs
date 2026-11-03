//! 图样字典（T.88 §6.7、§7.4.4）和半色调区域（§6.6、§7.4.5）。
//!
//! 图样字典是一排等大的小位图（图样），整条拼成一张“集合位图”用通用区域编码。半色调区域
//! 先解出一张灰度图（每格一个图样编号，按灰度码位平面编码，附录 C.5），再按一个可以倾斜的
//! 网格把对应的图样逐格合成到区域上。

use super::bitmap::{self, Bitmap, Op};
use super::generic;
use super::mq::{Mq, contexts};
use super::{RegionInfo, be32};

/// 图样字典段（类型 16）的数据：解出各图样（§6.7.5）。
pub(super) fn pattern_dict(data: &[u8]) -> Result<Vec<Bitmap>, String> {
    // [协议] 标志 1 字节、HDPW 1 字节、HDPH 1 字节、GRAYMAX 4 字节，共 7 字节（§7.4.4.1）
    const HEAD: usize = 7;
    if data.len() < HEAD {
        return Err("图样字典段太短".into());
    }
    let flags = data[0];
    // [协议] 标志：位 0 HDMMR，位 1–2 HDTEMPLATE（§7.4.4.1.1）
    let is_mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    let (pw, ph) = (usize::from(data[1]), usize::from(data[2])); // [协议] HDPW、HDPH（§7.4.4.1.2–3）
    let graymax = be32(data, 3)? as usize; // [协议] GRAYMAX 在第 3 字节起（§7.4.4.1.4）
    if pw == 0 || ph == 0 {
        return Err("图样宽或高为 0".into());
    }
    // §6.7.5 步骤 1：集合位图宽 (GRAYMAX + 1) × HDPW、高 HDPH
    let count = graymax.checked_add(1).ok_or("GRAYMAX 溢出")?;
    let width = count.checked_mul(pw).ok_or("集合位图太宽")?;
    let body = &data[HEAD..];
    let collective = if is_mmr {
        generic::mmr(body, width, ph)?.0
    } else {
        // [协议] GBAT：A1 = (−HDPW, 0)，模板 0 另三个是 (−3, −1)、(2, −2)、(−2, −2)
        // （§6.7.5 步骤 1 表 27）
        let a1 = i8::try_from(-(pw as i64))
            .map_err(|_| format!("HDPW = {pw} 超出 AT 像素偏移能表示的范围（−128..）"))?;
        let at = [(a1, 0), (-3, -1), (2, -2), (-2, -2)];
        let mut mq = Mq::new(body);
        let mut cx = contexts(generic::context_count(template));
        generic::decode(
            &mut mq,
            &mut cx,
            &generic::Params {
                w: width,
                h: ph,
                template,
                tpgdon: false,
                at,
                skip: None,
            },
        )?
    };
    // 步骤 2：第 i 个图样是集合位图的第 i·HDPW 列起、宽 HDPW 的一块
    (0..count)
        .map(|i| collective.crop((i * pw) as i64, 0, pw, ph))
        .collect()
}

/// 半色调区域段头里的参数（§7.4.5.1，表 36 的 HMMR … HRY）。
struct Header {
    mmr: bool,
    template: u8,
    enable_skip: bool,
    op: Op,
    default_pixel: u8,
    /// HGW、HGH：网格的列数、行数。
    gw: usize,
    gh: usize,
    /// HGX、HGY：网格原点，单位 1/256 像素。
    gx: i64,
    gy: i64,
    /// HRX、HRY：网格向量，单位 1/256 像素。
    rx: i64,
    ry: i64,
}

/// [协议] 半色调区域段头：标志 1、HGW 4、HGH 4、HGX 4、HGY 4、HRX 2、HRY 2 字节（§7.4.5.1）
const HEADER_LEN: usize = 21;

fn header(data: &[u8]) -> Result<Header, String> {
    if data.len() < HEADER_LEN {
        return Err("半色调区域段太短".into());
    }
    let flags = data[0];
    let u16_at = |i: usize| i64::from(u16::from_be_bytes([data[i], data[i + 1]]));
    Ok(Header {
        // [协议] 标志：位 0 HMMR，位 1–2 HTEMPLATE，位 3 HENABLESKIP，位 4–6 HCOMBOP，
        // 位 7 HDEFPIXEL（§7.4.5.1.1）
        mmr: flags & 1 != 0,
        template: (flags >> 1) & 3,
        enable_skip: flags & 8 != 0,
        op: bitmap::Op::from_bits((flags >> 4) & 7)?,
        default_pixel: flags >> 7,
        // [协议] 各字段的偏移见 §7.4.5.1.2–5；HGX、HGY 是有符号 32 位，HRX、HRY 无符号 16 位
        gw: be32(data, 1)? as usize,
        gh: be32(data, 5)? as usize,
        gx: i64::from(be32(data, 9)? as i32),
        gy: i64::from(be32(data, 13)? as i32),
        rx: u16_at(17),
        ry: u16_at(19),
    })
}

impl Header {
    /// 第 mg 行、第 ng 列格子的左上角在区域里的位置（§6.6.5.2 步骤 5a，§6.6.5.1 步骤 1b）：
    /// x = (HGX + mg·HRY + ng·HRX) >> 8，y = (HGY + mg·HRX − ng·HRY) >> 8。
    fn cell(&self, mg: usize, ng: usize) -> (i64, i64) {
        let (m, n) = (mg as i64, ng as i64);
        // [协议] 坐标是 1/256 像素的定点数，算术右移 8 位取整（§6.6.5.2）
        let x = (self.gx + m * self.ry + n * self.rx) >> 8;
        let y = (self.gy + m * self.rx - n * self.ry) >> 8;
        (x, y)
    }

    /// HSKIP 位图（§6.6.5.1）：图样整个落在区域之外的格子为 1。
    fn skip(
        &self,
        region_w: usize,
        region_h: usize,
        pw: usize,
        ph: usize,
    ) -> Result<Bitmap, String> {
        let mut s = Bitmap::new(self.gw, self.gh, 0)?;
        let (bw, bh) = (region_w as i64, region_h as i64);
        for mg in 0..self.gh {
            for ng in 0..self.gw {
                let (x, y) = self.cell(mg, ng);
                if x + pw as i64 <= 0 || x >= bw || y + ph as i64 <= 0 || y >= bh {
                    s.set(ng, mg, 1);
                }
            }
        }
        Ok(s)
    }
}

/// 灰度图解码（附录 C.5）：`bpp` 个位平面，按灰度码编码，高位在前。返回每格的值，按行存放。
fn gray_image(
    data: &[u8],
    h: &Header,
    bpp: u32,
    skip: Option<&Bitmap>,
) -> Result<Vec<u32>, String> {
    let n = h.gw.checked_mul(h.gh).ok_or("灰度图尺寸溢出")?;
    // [推导] 每格一个 u32
    bitmap::charge(
        n.checked_mul(std::mem::size_of::<u32>())
            .ok_or("灰度图尺寸溢出")?,
    )?;
    let mut vals = vec![0u32; n];
    if bpp == 0 {
        // 只有一个图样：不编码任何位平面，全部是 0
        return Ok(vals);
    }
    // 上一个（更高的）位平面换算回二进制后的值，用于灰度码 → 二进制（步骤 3）
    let mut prev: Option<Bitmap> = None;
    // 算术编码时各位平面共用一个解码器和一组上下文（步骤 1、2 依次调用通用区域解码）
    let mut mq = Mq::new(data);
    let mut cx = contexts(if h.mmr {
        0
    } else {
        generic::context_count(h.template)
    });
    let mut pos = 0usize;
    for _ in 0..bpp {
        let mut plane = if h.mmr {
            // MMR：各位平面的码流首尾相接，按用掉的字节数前进
            let rest = data.get(pos..).unwrap_or(&[]);
            let (b, used) = generic::mmr(rest, h.gw, h.gh)?;
            pos = pos.saturating_add(skip_eofb(rest, used));
            b
        } else {
            // [协议] 步骤 1：TPGDON = 0，USESKIP = HENABLESKIP，GBAT 用各模板的默认值
            generic::decode(
                &mut mq,
                &mut cx,
                &generic::Params {
                    w: h.gw,
                    h: h.gh,
                    template: h.template,
                    tpgdon: false,
                    at: generic::default_at(h.template),
                    skip,
                },
            )?
        };
        // 步骤 3：GSPLANES[j] = GSPLANES[j] XOR GSPLANES[j + 1]
        if let Some(p) = &prev {
            for (a, &b) in plane.data.iter_mut().zip(&p.data) {
                *a ^= b;
            }
        }
        // 步骤 4：GSVALS = Σ GSPLANES[j] · 2^j；从高位往低位依次左移累加
        for (v, &b) in vals.iter_mut().zip(&plane.data) {
            *v = (*v << 1) | u32::from(b);
        }
        prev = Some(plane);
    }
    Ok(vals)
}

/// [协议] EOFB：两个 T.6 的 EOL（000000000001），共 24 位（T.6 §2.4.3）
const EOFB: u32 = 0x001001;
/// [协议] EOFB 的位数
const EOFB_BITS: usize = 24;

/// MMR 位平面之后的 EOFB（T.88 §6.2.6：码流长度事先不知道时以 EOFB 结尾，灰度图的各位平面
/// 就是这样首尾相接的）。`generic::mmr` 在解够行数后就停，不读后面的 EOFB，返回的字节数只到
/// 最后一行所在的字节为止，EOFB 从这个字节中间某一位开始。这里在该字节内找 EOFB 的起点，
/// 返回读掉 EOFB 并补齐到字节边界后的字节数（下一个位平面从字节边界开始）；找不到时原样返回。
///
/// [推导] 起点唯一：若在真正起点 p 之前同一字节内的 p′ 也匹配，p′ + 11 处应是 1，但
/// p ≤ p′ + 11 < p + 11 落在 EOFB 前 11 个 0 里，矛盾。
fn skip_eofb(data: &[u8], used: usize) -> usize {
    let bit = |i: usize| (data.get(i / 8).copied().unwrap_or(0) >> (7 - i % 8)) & 1;
    // [推导] 最后一行可能正好在上一个字节末尾结束，所以从 used − 1 字节的第 0 位找到 used 字节的第 0 位
    let start = used.saturating_sub(1) * 8;
    for p in start..=used * 8 {
        if p + EOFB_BITS > data.len() * 8 {
            break;
        }
        let v = (p..p + EOFB_BITS).fold(0u32, |v, i| (v << 1) | u32::from(bit(i)));
        if v == EOFB {
            return (p + EOFB_BITS).div_ceil(8);
        }
    }
    used
}

/// 半色调区域段（类型 20、22、23）区域信息之后的数据；`patterns` 是引用的图样字典。
pub(super) fn region(
    data: &[u8],
    info: &RegionInfo,
    patterns: &[Bitmap],
) -> Result<Bitmap, String> {
    let h = header(data)?;
    let body = &data[HEADER_LEN..];
    let first = patterns.first().ok_or("引用的图样字典是空的")?;
    let (pw, ph) = (first.w, first.h);
    // §6.6.5.2 步骤 1：整个区域先填 HDEFPIXEL
    let mut out = Bitmap::new(info.w, info.h, h.default_pixel)?;
    // 步骤 2：HENABLESKIP 时算 HSKIP（只对算术编码有意义，MMR 不跳过）
    let skip = if h.enable_skip && !h.mmr {
        Some(h.skip(info.w, info.h, pw, ph)?)
    } else {
        None
    };
    // 步骤 3：HBPP = ⌈log2(HNUMPATS)⌉
    // [推导] n ≥ 1 时 ⌈log2 n⌉ 等于 n − 1 的二进制位数
    let bpp = usize::BITS - (patterns.len() - 1).leading_zeros();
    // 步骤 4：灰度图
    let gray = gray_image(body, &h, bpp, skip.as_ref())?;
    // 步骤 5：逐格把图样 HPATS[GI[ng, mg]] 按 HCOMBOP 合成到区域上
    for mg in 0..h.gh {
        for ng in 0..h.gw {
            // [策略] 编号超出图样个数的格子（码流有误）不画，保留缺省像素
            let Some(pat) = patterns.get(gray[mg * h.gw + ng] as usize) else {
                continue;
            };
            let (x, y) = h.cell(mg, ng);
            out.compose(pat, x, y, h.op);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(gx: i64, gy: i64, rx: i64, ry: i64, gw: usize, gh: usize) -> Header {
        Header {
            mmr: false,
            template: 0,
            enable_skip: true,
            op: Op::Or,
            default_pixel: 0,
            gw,
            gh,
            gx,
            gy,
            rx,
            ry,
        }
    }

    /// 网格坐标：不旋转时 HRX = 图样宽 × 256、HRY = 0，格子横排；负原点向下取整
    #[test]
    fn grid_cells() {
        // [策略] 测试数据：4 像素宽的图样，原点 (−1.5, 0)
        let h = hdr(-384, 0, 4 * 256, 0, 3, 2);
        assert_eq!(h.cell(0, 0), (-2, 0));
        assert_eq!(h.cell(0, 2), (6, 0));
        assert_eq!(h.cell(1, 1), (2, 4));
        // 有 HRY 时：沿行往右上倾斜，沿列往右下
        let t = hdr(0, 0, 256, 128, 2, 2);
        assert_eq!(t.cell(0, 1), (1, -1));
        assert_eq!(t.cell(1, 0), (0, 1));
    }

    #[test]
    fn skip_cells_outside() {
        // [策略] 测试用的分配上限，够这几张小图
        bitmap::set_budget(1 << 16);
        // 3 列 1 行、每格 4 像素宽，区域只有 5 像素宽：第 0 格露出一半，第 2 格在区域外
        let h = hdr(-512, 0, 4 * 256, 0, 3, 1);
        let s = h.skip(5, 4, 4, 4).unwrap();
        assert_eq!(s.data, vec![0, 0, 1]);
    }

    /// 把 "0101…" 拼成字节，末尾补 0
    fn pack(bits: &str) -> Vec<u8> {
        let v: Vec<u8> = bits.bytes().map(|c| c - b'0').collect();
        // [协议] 一字节 8 位
        v.chunks(8)
            .map(|c| {
                c.iter()
                    .enumerate()
                    .fold(0u8, |a, (i, &b)| a | (b << (7 - i)))
            })
            .collect()
    }

    #[test]
    fn eofb_after_plane() {
        let eofb = "000000000001000000000001";
        // 最后一行在第 0 字节第 3 位结束：EOFB 占到第 26 位，下一平面从第 4 字节开始
        let d = pack(&format!("101{eofb}00000"));
        assert_eq!(skip_eofb(&d, 1), 4);
        // 最后一行正好用完第 0 字节：EOFB 从第 1 字节开始，占满 3 字节
        let d = pack(&format!("10000001{eofb}"));
        assert_eq!(skip_eofb(&d, 1), 4);
        // 后面没有 EOFB：原样返回
        assert_eq!(skip_eofb(&[0xFF, 0xFF, 0xFF, 0xFF], 1), 1);
    }

    /// 只有一个图样时 HBPP = 0，不解码任何位平面
    #[test]
    fn single_pattern_needs_no_planes() {
        bitmap::set_budget(1 << 16);
        let h = hdr(0, 0, 256, 0, 2, 2);
        assert_eq!(gray_image(&[], &h, 0, None).unwrap(), vec![0; 4]);
    }
}
