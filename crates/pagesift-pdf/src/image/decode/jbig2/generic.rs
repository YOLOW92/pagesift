//! 通用区域解码过程（T.88 §6.2）：算术编码的 4 种模板、典型预测（TPGDON）、跳过位图（USESKIP），
//! 以及 MMR 编码（§6.2.6，就是 T.6，交给 CCITT 解码）；通用区域段的数据格式（§7.4.6）。

use super::RegionInfo;
use super::bitmap::{self, Bitmap};
use super::mq::{Cx, Mq, contexts};
use crate::image::decode::ccitt;
use crate::image::tiff::Ccitt;

/// 通用区域解码的参数（T.88 表 2）。
pub(super) struct Params<'a> {
    pub(super) w: usize,
    pub(super) h: usize,
    /// GBTEMPLATE，0–3。
    pub(super) template: u8,
    pub(super) tpgdon: bool,
    /// [协议] GBAT：自适应模板像素相对当前像素的偏移 (x, y)；模板 0 用 4 个，其余用第 1 个
    /// （T.88 §6.2.5.3）。
    /// 段里写的是有符号字节，图样字典的 A1 = (−HDPW, 0) 可到 −255，所以用 i16。
    pub(super) at: [(i16, i16); 4],
    /// USESKIP 时的 SKIP 位图：为 1 的像素不解码、置 0。
    pub(super) skip: Option<&'a Bitmap>,
}

/// 各模板的上下文个数（上下文位数：16、13、10、10，T.88 图 3–6）。
pub(super) fn context_count(template: u8) -> usize {
    // [协议] 模板 0–3 的上下文分别是 16、13、10、10 位
    1 << [16, 13, 10, 10][usize::from(template & 3)]
}

/// [协议] 各模板默认的 AT 像素（T.88 §6.2.5.3 图 3–6 里 A1–A4 的位置）
pub(super) fn default_at(template: u8) -> [(i16, i16); 4] {
    match template {
        // [协议] 模板 0 有 A1–A4，模板 1–3 只有 A1，其余位置不用（T.88 图 3–6）
        0 => [(3, -1), (-3, -1), (2, -2), (-2, -2)],
        1 => [(3, -1), (0, 0), (0, 0), (0, 0)],
        _ => [(2, -1), (0, 0), (0, 0), (0, 0)],
    }
}

/// 一个模板像素：固定偏移，或第几个 AT 像素。
#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum Px {
    At(usize),
    Fixed(i8, i8),
}

/// 测试用：按 TEMPLATES 逐像素算上下文，与滑动分组的结果对照。
/// [协议] AT 像素至多 4 个（T.88 §6.2.5.3）
#[cfg(test)]
pub(super) fn context_slow(b: &Bitmap, t: usize, at: &[(i16, i16); 4], x: i64, y: i64) -> usize {
    TEMPLATES[t].iter().enumerate().fold(0, |c, (bit, &px)| {
        let (dx, dy) = match px {
            At(k) => (i64::from(at[k].0), i64::from(at[k].1)),
            Fixed(dx, dy) => (i64::from(dx), i64::from(dy)),
        };
        c | usize::from(b.get(x + dx, y + dy)) << bit
    })
}

/// 测试用：按 GROUPS、AT_BITS 算上下文。
/// [协议] AT 像素至多 4 个（T.88 §6.2.5.3）
#[cfg(test)]
pub(super) fn context_fast(b: &Bitmap, t: usize, at: &[(i16, i16); 4], x: i64, y: i64) -> usize {
    let mut c = 0;
    for g in GROUPS[t] {
        for i in 0..g.n {
            let v = b.get(x + g.left + i64::from(i), y + g.dy);
            c |= usize::from(v) << (g.shift + g.n - 1 - i);
        }
    }
    for &(k, bit) in AT_BITS[t] {
        c |= usize::from(b.get(x + i64::from(at[k].0), y + i64::from(at[k].1))) << bit;
    }
    c
}

#[cfg(test)]
use Px::{At, Fixed};

/// [协议] 各模板的像素，按上下文的位从低到高（T.88 图 3–6；与参考实现的位序一致，
/// TPGDON 的伪像素上下文 SLTP 依赖这个位序）。解码用由它归并出的 [`GROUPS`]、[`AT_BITS`]。
#[cfg(test)]
pub(super) const TEMPLATES: [&[Px]; 4] = [
    &[
        Fixed(-1, 0),
        Fixed(-2, 0),
        Fixed(-3, 0),
        Fixed(-4, 0),
        At(0),
        Fixed(2, -1),
        Fixed(1, -1),
        Fixed(0, -1),
        Fixed(-1, -1),
        Fixed(-2, -1),
        At(1),
        At(2),
        Fixed(1, -2),
        Fixed(0, -2),
        Fixed(-1, -2),
        At(3),
    ],
    &[
        Fixed(-1, 0),
        Fixed(-2, 0),
        Fixed(-3, 0),
        At(0),
        Fixed(2, -1),
        Fixed(1, -1),
        Fixed(0, -1),
        Fixed(-1, -1),
        Fixed(-2, -1),
        Fixed(2, -2),
        Fixed(1, -2),
        Fixed(0, -2),
        Fixed(-1, -2),
    ],
    &[
        Fixed(-1, 0),
        Fixed(-2, 0),
        At(0),
        Fixed(1, -1),
        Fixed(0, -1),
        Fixed(-1, -1),
        Fixed(-2, -1),
        Fixed(1, -2),
        Fixed(0, -2),
        Fixed(-1, -2),
    ],
    &[
        Fixed(-1, 0),
        Fixed(-2, 0),
        Fixed(-3, 0),
        Fixed(-4, 0),
        At(0),
        Fixed(1, -1),
        Fixed(0, -1),
        Fixed(-1, -1),
        Fixed(-2, -1),
        Fixed(-3, -1),
    ],
];

/// [协议] TPGDON 的伪像素上下文 SLTP（T.88 §6.2.5.7 步骤 3b，图 8–11）
const SLTP: [usize; 4] = [0x9B25, 0x0795, 0x00E5, 0x0195];

/// 算术编码的通用区域解码（T.88 §6.2.5.7）。`cx` 至少 [`context_count`] 个，
/// 符号字典里多个符号共用同一组上下文，所以由调用方持有。
pub(super) fn decode(mq: &mut Mq, cx: &mut [Cx], p: &Params) -> Result<Bitmap, String> {
    let t = usize::from(p.template & 3); // [协议] GBTEMPLATE 只有 0–3（T.88 §7.4.6.2）
    if cx.len() < context_count(p.template) {
        return Err("通用区域的上下文表太小".into());
    }
    let offs: Vec<(i64, i64)> =
        p.at.iter()
            .map(|&(x, y)| (i64::from(x), i64::from(y)))
            .collect();
    let mut out = Bitmap::new(p.w, p.h, 0)?;
    let mut ltp = 0u8;
    for y in 0..p.h {
        bitmap::check_time()?;
        if p.tpgdon {
            ltp ^= mq.decode(&mut cx[SLTP[t]]);
            if ltp == 1 {
                // 这一行和上一行相同（第一行的“上一行”全是 0）
                if y > 0 {
                    let (above, row) = out.data.split_at_mut(y * p.w);
                    row[..p.w].copy_from_slice(&above[(y - 1) * p.w..]);
                }
                continue;
            }
        }
        let yy = y as i64;
        // 固定像素按行分组滑动：每组是同一行上连续的几个像素，左边的在高位；
        // 每前进一列，组里移进右边新的一个像素。AT 像素逐个取。
        let groups = GROUPS[t];
        // [协议] 模板的固定像素至多分布在 3 行（y − 2、y − 1、y，T.88 图 3–6），每行一组
        let mut win = [0usize; 3];
        for (g, w) in groups.iter().zip(&mut win) {
            for i in 0..g.n {
                *w = (*w << 1) | usize::from(out.get(g.left + i64::from(i), yy + g.dy));
            }
        }
        for x in 0..p.w {
            let xx = x as i64;
            if !p.skip.is_some_and(|s| s.get(xx, yy) == 1) {
                let mut ctx = 0usize;
                for (g, &w) in groups.iter().zip(&win) {
                    ctx |= w << g.shift;
                }
                for &(k, bit) in AT_BITS[t] {
                    ctx |= usize::from(out.get(xx + offs[k].0, yy + offs[k].1)) << bit;
                }
                if mq.decode(&mut cx[ctx]) != 0 {
                    out.set(x, y, 1);
                }
            }
            for (g, w) in groups.iter().zip(&mut win) {
                let next = out.get(xx + 1 + g.left + i64::from(g.n) - 1, yy + g.dy);
                *w = ((*w << 1) | usize::from(next)) & ((1 << g.n) - 1);
            }
        }
    }
    Ok(out)
}

/// 模板里同一行上连续的固定像素：行偏移、最左像素的列偏移、个数、在上下文里的最低位。
struct Group {
    dy: i64,
    left: i64,
    n: u32,
    shift: u32,
}

const fn g(dy: i64, left: i64, n: u32, shift: u32) -> Group {
    Group { dy, left, n, shift }
}

/// [推导] 由 TEMPLATES 按行归并出的分组（测试里逐位核对两者一致）
const GROUPS: [&[Group]; 4] = [
    &[g(-2, -1, 3, 12), g(-1, -2, 5, 5), g(0, -4, 4, 0)],
    &[g(-2, -1, 4, 9), g(-1, -2, 5, 4), g(0, -3, 3, 0)],
    &[g(-2, -1, 3, 7), g(-1, -2, 4, 3), g(0, -2, 2, 0)],
    &[g(-1, -3, 5, 5), g(0, -4, 4, 0)],
];

/// [推导] 各模板 AT 像素在上下文里的位：(第几个 AT, 位)，同样由 TEMPLATES 得出
const AT_BITS: [&[(usize, u32)]; 4] = [
    &[(0, 4), (1, 10), (2, 11), (3, 15)],
    &[(0, 3)],
    &[(0, 2)],
    &[(0, 4)],
];

/// MMR 编码的通用区域（T.88 §6.2.6）：T.6 码流，黑像素是 1。返回位图和用掉的字节数。
pub(super) fn mmr(data: &[u8], w: usize, h: usize) -> Result<(Bitmap, usize), String> {
    if w == 0 || h == 0 {
        return Ok((Bitmap::new(w, h, 0)?, 0));
    }
    let p = Ccitt {
        k: -1,
        columns: u32::try_from(w).map_err(|_| "MMR 区域太宽")?,
        rows: u32::try_from(h).map_err(|_| "MMR 区域太高")?,
        black_is_1: true,
        byte_align: false,
    };
    let mut out = Bitmap::new(w, h, 0)?;
    // 解码器自己的输出也算进上限
    let left = w * h;
    bitmap::charge(left)?;
    let (r, used) = ccitt::decode_counted(data, &p, left)?;
    for (o, &v) in out.data.iter_mut().zip(&r.data) {
        // [协议] black_is_1 时黑解成 255
        *o = u8::from(v == 255);
    }
    Ok((out, used))
}

/// 通用区域段（类型 36、38、39）区域信息之后的数据（T.88 §7.4.6）。
pub(super) fn segment(data: &[u8], info: &RegionInfo) -> Result<Bitmap, String> {
    let flags = *data.first().ok_or("通用区域段太短")?;
    // [协议] 标志位：位 0 MMR，位 1–2 GBTEMPLATE，位 3 TPGDON，位 4 EXTTEMPLATE（§7.4.6.2）
    let is_mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3; // [协议] 位 1–2 GBTEMPLATE
    let tpgdon = flags & 8 != 0; // [协议] 位 3 TPGDON
    // [协议] 位 4 EXTTEMPLATE
    if flags & 0x10 != 0 {
        return Err("不支持扩展模板（EXTTEMPLATE）".into());
    }
    if is_mmr {
        return mmr(&data[1..], info.w, info.h).map(|(b, _)| b);
    }
    // [协议] AT 像素：模板 0 是 4 对，其余 1 对，每个坐标一个有符号字节（§7.4.6.3）
    let pairs = if template == 0 { 4 } else { 1 };
    let at_bytes = data
        .get(1..1 + 2 * pairs)
        .ok_or("通用区域段的 AT 像素不全")?;
    let mut at = default_at(template);
    for (k, a) in at.iter_mut().take(pairs).enumerate() {
        *a = (
            i16::from(at_bytes[2 * k] as i8),
            i16::from(at_bytes[2 * k + 1] as i8),
        );
    }
    let mut mq = Mq::new(&data[1 + 2 * pairs..]);
    let mut cx = contexts(context_count(template));
    decode(
        &mut mq,
        &mut cx,
        &Params {
            w: info.w,
            h: info.h,
            template,
            tpgdon,
            at,
            skip: None,
        },
    )
}
