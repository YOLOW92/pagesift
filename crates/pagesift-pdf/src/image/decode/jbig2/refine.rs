//! 通用细化区域解码过程（T.88 §6.3）和细化区域段（§7.4.7）。
//!
//! 细化是“参照一张已有的位图，逐像素算术解码出一张新位图”：上下文由新位图里已解出的邻近像素
//! 和参照位图里对应位置周围的像素共同组成。供符号字典（REFAGG）、文本区域（SBREFINE）和
//! 细化区域段调用。

use super::RegionInfo;
use super::bitmap::Bitmap;
use super::mq::{Cx, Mq, contexts};

/// 通用细化区域解码的参数（T.88 表 6）。
pub(super) struct Params<'a> {
    pub(super) w: usize,
    pub(super) h: usize,
    /// GRTEMPLATE，0 或 1。
    pub(super) template: u8,
    /// GRREFERENCE：参照位图。
    pub(super) reference: &'a Bitmap,
    /// GRREFERENCEDX、GRREFERENCEDY：参照位图相对本区域的偏移。
    pub(super) dx: i32,
    pub(super) dy: i32,
    pub(super) tpgron: bool,
    /// GRAT：模板 0 的两个 AT 像素（第一个在本区域、第二个在参照图上）。
    pub(super) at: [(i8, i8); 2],
}

/// 各模板的上下文个数（模板 0 是 13 位，模板 1 是 10 位）。
pub(super) fn context_count(template: u8) -> usize {
    // [协议] GRTEMPLATE 0 的上下文 13 位、1 的 10 位（T.88 图 12、13）
    if template == 0 { 1 << 13 } else { 1 << 10 }
}

/// 模板里的一个像素：在正在解码的位图上（相对当前像素），或在参照位图上（相对参照图里
/// 与当前像素对应的那个像素，即 (x − GRREFERENCEDX, y − GRREFERENCEDY)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Px {
    /// 本区域的固定像素。
    Cur(i8, i8),
    /// 参照图的固定像素。
    Ref(i8, i8),
    /// 本区域的 AT 像素 A1（GRATX1, GRATY1）。
    CurAt,
    /// 参照图的 AT 像素 A2（GRATX2, GRATY2）。
    RefAt,
}

use Px::{Cur, CurAt, Ref, RefAt};

/// [协议] 两个模板的像素（T.88 §6.3.5.3 图 12、13），按上下文的位从低到高排列：先本区域、
/// 后参照图，各自按光栅顺序（自上而下、自左而右）。
///
/// 位序本身不影响解码结果（上下文只是一组状态的编号），唯一依赖位序的是 TPGRON 的伪像素
/// 上下文：它借用“只有参照图中心像素为 1”那个真实上下文（见 [`sltp_context`]）。
const TEMPLATES: [&[Px]; 2] = [
    // 图 12，GRTEMPLATE = 0：本区域 4 个（含 A1）、参照图 9 个（含 A2）
    &[
        CurAt,
        Cur(0, -1),
        Cur(1, -1),
        Cur(-1, 0),
        RefAt,
        Ref(0, -1),
        Ref(1, -1),
        Ref(-1, 0),
        Ref(0, 0),
        Ref(1, 0),
        Ref(-1, 1),
        Ref(0, 1),
        Ref(1, 1),
    ],
    // 图 13，GRTEMPLATE = 1：本区域 4 个、参照图 6 个，没有 AT 像素
    &[
        Cur(-1, -1),
        Cur(0, -1),
        Cur(1, -1),
        Cur(-1, 0),
        Ref(0, -1),
        Ref(-1, 0),
        Ref(0, 0),
        Ref(1, 0),
        Ref(0, 1),
        Ref(1, 1),
    ],
];

/// [协议] GRTEMPLATE 0 的默认 AT 像素：A1 = (−1, −1)（本区域）、A2 = (−1, −1)（参照图），
/// 即图 12 里 A1、A2 的位置（§6.3.5.3）
const DEFAULT_AT: [(i8, i8); 2] = [(-1, -1), (-1, -1)];

/// TPGRON 解 SLTP 用的伪像素上下文（T.88 §6.3.5.6 步骤 3a，图 14、15）：模板像素里只有
/// 参照图中心像素（Ref(0, 0)）为 1、其余全为 0 时的上下文值。
/// [推导] 由 [`TEMPLATES`] 里 Ref(0, 0) 所在的位得出，位序改了也自动跟着变。
fn sltp_context(t: usize) -> usize {
    let bit = TEMPLATES[t]
        .iter()
        .position(|&p| p == Ref(0, 0))
        .unwrap_or(0);
    1 << bit
}

/// 通用细化区域解码（§6.3.5.6）。`cx` 至少 [`context_count`] 个，由调用方持有（文本区域、
/// 符号字典里多次细化共用）。
pub(super) fn decode(mq: &mut Mq, cx: &mut [Cx], p: &Params) -> Result<Bitmap, String> {
    if p.template > 1 {
        return Err(format!("无效的 GRTEMPLATE {}", p.template));
    }
    let t = usize::from(p.template);
    if cx.len() < context_count(p.template) {
        return Err("通用细化区域的上下文表太小".into());
    }
    let r = p.reference;
    let (rdx, rdy) = (i64::from(p.dx), i64::from(p.dy));
    // 把模板展开成 (是否参照图, dx, dy)
    let offs: Vec<(bool, i64, i64)> = TEMPLATES[t]
        .iter()
        .map(|&px| match px {
            Cur(x, y) => (false, i64::from(x), i64::from(y)),
            Ref(x, y) => (true, i64::from(x), i64::from(y)),
            CurAt => (false, i64::from(p.at[0].0), i64::from(p.at[0].1)),
            RefAt => (true, i64::from(p.at[1].0), i64::from(p.at[1].1)),
        })
        .collect();
    let sltp = sltp_context(t);
    let mut out = Bitmap::new(p.w, p.h, 0)?;
    // §6.3.5.6 步骤 1：LTP = 0
    let mut ltp = 0u8;
    for y in 0..p.h {
        super::bitmap::check_time()?;
        let yy = y as i64;
        // 参照图里与本行对应的行
        let ry = yy - rdy;
        if p.tpgron {
            // 步骤 3a：解 SLTP，LTP ^= SLTP
            ltp ^= mq.decode(&mut cx[sltp]);
        }
        for x in 0..p.w {
            let xx = x as i64;
            let rx = xx - rdx;
            if ltp == 1 {
                // 步骤 3c：典型预测。参照图里以对应像素为中心的 3×3 全同时，本像素直接取该值
                // （TPGRPIX = 1，不解码）；否则照常解码
                if let Some(v) = uniform_3x3(r, rx, ry) {
                    if v != 0 {
                        out.set(x, y, 1);
                    }
                    continue;
                }
            }
            let mut ctx = 0usize;
            for (bit, &(is_ref, ox, oy)) in offs.iter().enumerate() {
                let v = if is_ref {
                    r.get(rx + ox, ry + oy)
                } else {
                    out.get(xx + ox, yy + oy)
                };
                ctx |= usize::from(v) << bit;
            }
            if mq.decode(&mut cx[ctx]) != 0 {
                out.set(x, y, 1);
            }
        }
    }
    Ok(out)
}

/// 参照图中以 (x, y) 为中心的 3×3 邻域（图外当 0）全是同一个值时返回它（§6.3.5.6 的 TPGRVAL）。
fn uniform_3x3(r: &Bitmap, x: i64, y: i64) -> Option<u8> {
    let v = r.get(x, y);
    // [协议] 3×3 邻域：相对中心 −1..=1（§6.3.5.6 步骤 3c）
    for oy in -1..=1 {
        for ox in -1..=1 {
            if r.get(x + ox, y + oy) != v {
                return None;
            }
        }
    }
    Some(v)
}

/// 细化区域段（类型 40、42、43）区域信息之后的数据（§7.4.7）。`reference` 已按区域裁好
/// （引用中间区域时就是它的位图），GRREFERENCEDX/DY 为 0。
pub(super) fn segment(
    data: &[u8],
    info: &RegionInfo,
    reference: &Bitmap,
) -> Result<Bitmap, String> {
    let flags = *data.first().ok_or("细化区域段太短")?;
    // [协议] 标志：位 0 GRTEMPLATE，位 1 TPGRON（§7.4.7.2）
    let template = flags & 1;
    let tpgron = flags & 2 != 0;
    let mut at = DEFAULT_AT;
    // [协议] 标志 1 字节之后，模板 0 有 GRATX1、GRATY1、GRATX2、GRATY2 各 1 个有符号字节
    // （§7.4.7.3）；模板 1 没有
    let head = if template == 0 {
        let b = data.get(1..5).ok_or("细化区域段的 AT 像素不全")?;
        at = [(b[0] as i8, b[1] as i8), (b[2] as i8, b[3] as i8)];
        5 // [协议] 标志 1 字节 + AT 4 字节
    } else {
        1
    };
    let mut mq = Mq::new(&data[head..]);
    let mut cx = contexts(context_count(template));
    decode(
        &mut mq,
        &mut cx,
        &Params {
            w: info.w,
            h: info.h,
            template,
            reference,
            dx: 0,
            dy: 0,
            tpgron,
            at,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [协议] 伪像素上下文：模板 0 共 13 位、模板 1 共 10 位，只有参照图中心那一位为 1（图 14、15）
    #[test]
    fn sltp_is_reference_center_only() {
        for (t, px) in TEMPLATES.iter().enumerate() {
            let ctx = sltp_context(t);
            assert_eq!(ctx.count_ones(), 1);
            assert!(ctx < context_count(t as u8));
            assert_eq!(px[ctx.trailing_zeros() as usize], Ref(0, 0));
        }
        // [协议] 模板像素个数等于上下文位数（图 12 的 13 个、图 13 的 10 个）
        assert_eq!(TEMPLATES[0].len(), 13);
        assert_eq!(TEMPLATES[1].len(), 10);
    }

    #[test]
    fn uniform_neighbourhood() {
        // [策略] 测试用的分配上限，够这几张小图
        super::super::bitmap::set_budget(1 << 16);
        let mut b = Bitmap::new(4, 4, 0).unwrap();
        assert_eq!(uniform_3x3(&b, 1, 1), Some(0));
        b.set(3, 3, 1);
        assert_eq!(uniform_3x3(&b, 1, 1), Some(0));
        assert_eq!(uniform_3x3(&b, 2, 2), None);
        // 图外当 0：边上的黑像素邻域里总有图外像素，不能算全同
        let full = Bitmap::new(2, 2, 1).unwrap();
        assert_eq!(uniform_3x3(&full, 0, 0), None);
    }
}
