//! 符号字典解码过程（T.88 §6.5）和符号字典段（§7.4.2）。
//!
//! 新符号按高度类解出（§6.5.5）：算术编码时每个符号是一个通用区域（§6.5.8.1），霍夫曼编码时一个
//! 高度类合成一张集合位图（§6.5.9）；细化/聚合编码（REFAGG）时每个符号由细化或文本区域解码
//! 过程得到（§6.5.8.2）。最后按导出标志游程选出导出的符号（§6.5.10）。

mod source;
#[cfg(test)]
mod tests;

use std::rc::Rc;

use super::bitmap::{self, Bitmap, Op};
use super::generic;
use super::huffman::Table;
use super::mq::{Cx, contexts};
use super::refine;
use super::text::coder::{Field, code_len};
use super::text::{self, Corner, Symbols};
use source::{ArithDict, DictField, DictSource, HuffDict};

/// 留给后续符号字典沿用的算术解码上下文（§7.4.2.1.1 位 9“位图编码上下文保留”；后续字典的位 8
/// “位图编码上下文沿用”时取用，§7.4.2.2 步骤 3）。
#[derive(Debug)]
pub(super) struct Retained {
    /// 通用区域的上下文（按 SDTEMPLATE 的大小），霍夫曼编码的字典没有。
    gb: Vec<Cx>,
    /// 通用细化区域的上下文（按 SDRTEMPLATE 的大小），不用 REFAGG 的字典没有。
    gr: Vec<Cx>,
}

/// 符号字典段的头（§7.4.2.1）。
struct Header {
    huff: bool,
    refagg: bool,
    /// SDHUFFDH、SDHUFFDW、SDHUFFBMSIZE、SDHUFFAGGINST 的表选择值。
    sel_dh: u16,
    sel_dw: u16,
    sel_bmsize: u16,
    sel_agginst: u16,
    ctx_used: bool,
    ctx_retained: bool,
    template: u8,
    rtemplate: u8,
    at: [(i16, i16); 4], // [协议] SDAT 至多 4 个（§7.4.2.1.2）
    rat: [(i8, i8); 2],
    /// SDNUMNEWSYMS（SDNUMEXSYMS 只用于核对，导出个数以游程为准）。
    num_new: u32,
}

fn header(data: &[u8]) -> Result<(Header, &[u8]), String> {
    let f = data.get(0..2).ok_or("符号字典段太短")?; // [协议] 标志 2 字节（§7.4.2.1.1）
    let f = u16::from_be_bytes([f[0], f[1]]);
    let mut h = Header {
        // [协议] 位 0 SDHUFF、位 1 SDREFAGG、位 2–3 SDHUFFDH、位 4–5 SDHUFFDW、位 6 SDHUFFBMSIZE、
        // 位 7 SDHUFFAGGINST、位 8 上下文沿用、位 9 上下文保留、位 10–11 SDTEMPLATE、位 12 SDRTEMPLATE
        // （§7.4.2.1.1）
        huff: f & 1 != 0,
        refagg: f & 2 != 0,
        sel_dh: (f >> 2) & 3,
        sel_dw: (f >> 4) & 3,
        sel_bmsize: (f >> 6) & 1,
        sel_agginst: (f >> 7) & 1,
        ctx_used: f & 0x100 != 0,
        ctx_retained: f & 0x200 != 0,
        template: ((f >> 10) & 3) as u8,
        rtemplate: ((f >> 12) & 1) as u8,
        at: generic::default_at(((f >> 10) & 3) as u8),
        rat: [(0, 0); 2],
        num_new: 0,
    };
    let mut pos = 2; // [协议] 标志之后
    if !h.huff {
        // [协议] AT 像素：SDTEMPLATE 0 是 4 对，其余 1 对，每个坐标一个有符号字节（§7.4.2.1.2）
        let pairs = if h.template == 0 { 4 } else { 1 };
        let b = data
            .get(pos..pos + 2 * pairs)
            .ok_or("符号字典段的 AT 像素不全")?;
        for (k, a) in h.at.iter_mut().take(pairs).enumerate() {
            *a = (i16::from(b[2 * k] as i8), i16::from(b[2 * k + 1] as i8));
        }
        pos += 2 * pairs;
    }
    if h.refagg && h.rtemplate == 0 {
        // [协议] 细化 AT 像素 4 字节：SDRATX1、SDRATY1、SDRATX2、SDRATY2（§7.4.2.1.3）
        let b = data
            .get(pos..pos + 4)
            .ok_or("符号字典段的细化 AT 像素不全")?;
        h.rat = [(b[0] as i8, b[1] as i8), (b[2] as i8, b[3] as i8)];
        pos += 4; // [协议] 细化 AT 像素 4 字节
    }
    // [协议] SDNUMEXSYMS、SDNUMNEWSYMS 各 4 字节（§7.4.2.1.4、§7.4.2.1.5）
    h.num_new = super::be32(data, pos + 4)?;
    pos += 8; // [协议] SDNUMEXSYMS、SDNUMNEWSYMS 共 8 字节
    Ok((h, data.get(pos..).ok_or("符号字典段太短")?))
}

/// 符号字典段的数据。`input` 是引用的各符号字典导出的符号（SDINSYMS，按引用顺序），
/// `tables` 是引用的码表段（按引用顺序，自定义表从这里取），`prior` 是引用的最后一个符号字典
/// 保留下来的上下文。返回导出的符号（SDEXSYMS）和要保留的上下文（标志要求保留时）。
pub(super) fn dictionary(
    data: &[u8],
    input: &[Rc<Bitmap>],
    tables: &[Rc<Table>],
    prior: Option<&Retained>,
) -> Result<(Vec<Rc<Bitmap>>, Option<Retained>), String> {
    let (h, body) = header(data)?;
    let total = input
        .len()
        .checked_add(h.num_new as usize)
        .ok_or("符号数溢出")?;
    // 细化/聚合时的 SBSYMCODELEN（§6.5.8.2.3）；不用 REFAGG 时 IAID 用不到，码长取 0 不建上下文
    let id_len = if h.refagg { code_len(total) } else { 0 };
    let mut gb = if h.huff {
        Vec::new()
    } else {
        contexts(generic::context_count(h.template))
    };
    let mut gr = if h.refagg {
        contexts(refine::context_count(h.rtemplate))
    } else {
        Vec::new()
    };
    // §7.4.2.2 步骤 3：沿用上一字典留下的统计。模板必须相同（规范要求），大小对不上时
    // [策略] 当作没有可沿用的，从初值开始，不让整段失败
    if let Some(r) = prior.filter(|_| h.ctx_used) {
        if r.gb.len() == gb.len() {
            gb.clone_from(&r.gb);
        }
        if r.gr.len() == gr.len() {
            gr.clone_from(&r.gr);
        }
    }
    let (exported, gb, gr) = if h.huff {
        let mut src = HuffDict::new(&h, body, tables, id_len, gr)?;
        let ex = run(&mut src, &h, input)?;
        (ex, gb, src.into_gr())
    } else {
        let mut src = ArithDict::new(&h, body, id_len, gb, gr)?;
        let ex = run(&mut src, &h, input)?;
        let (gb, gr) = src.into_contexts();
        (ex, gb, gr)
    };
    // 保留的上下文一直存到解码结束：空字典段只有几十字节，却能留下上百 KB
    let retained = if h.ctx_retained {
        bitmap::charge(size_of_val(gb.as_slice()) + size_of_val(gr.as_slice()))?;
        Some(Retained { gb, gr })
    } else {
        None
    };
    Ok((exported, retained))
}

/// 解出新符号再导出（§6.5.5 步骤 4、5）。
fn run(
    src: &mut impl DictSource,
    h: &Header,
    input: &[Rc<Bitmap>],
) -> Result<Vec<Rc<Bitmap>>, String> {
    let new = new_symbols(src, h, input)?;
    export(src, input, &new)
}

/// [策略] 不含符号的高度类最多这么多个：规范没禁止空高度类，但码流读过末尾后算术解码会一直
/// 解出同样的值，空高度类不推进符号数，没有上限就会死循环
const MAX_EMPTY_CLASSES: usize = 1 << 16;

/// §6.5.5 步骤 4：逐个高度类解新符号。
fn new_symbols(
    src: &mut impl DictSource,
    h: &Header,
    input: &[Rc<Bitmap>],
) -> Result<Vec<Rc<Bitmap>>, String> {
    let num_new = h.num_new as usize;
    // 霍夫曼编码、不用 REFAGG 时整个高度类的位图最后一起解（步骤 4c）
    let collective = h.huff && !h.refagg;
    let mut new: Vec<Rc<Bitmap>> = Vec::new();
    let mut decoded = 0usize;
    let mut height = 0i64;
    let mut empty = 0usize;
    while decoded < num_new {
        // 4a：高度类的高度增量 HCDH
        let dh = src.int(DictField::Dh)?.ok_or("HCDH 不能是 OOB")?;
        height += i64::from(dh);
        let hc = usize::try_from(height).map_err(|_| "高度类的高度为负")?;
        let first = decoded;
        let mut width = 0i64;
        let mut widths = Vec::new();
        // 4b：逐个符号解宽度增量 DW，OOB 时高度类结束
        while let Some(dw) = src.int(DictField::Dw)? {
            if decoded >= num_new {
                return Err("高度类里的符号超过了 SDNUMNEWSYMS".into());
            }
            width += i64::from(dw);
            let w = usize::try_from(width).map_err(|_| "符号宽度为负")?;
            // [策略] 每个符号的簿记开销也计入分配上限：宽高为 0 的符号不占位图字节，
            // 不计的话几十字节的段头就能要求上亿个符号
            bitmap::charge(size_of::<Bitmap>())?;
            if collective {
                widths.push(w);
            } else {
                let bm = if h.refagg {
                    refagg(src, h, input, &new, w, hc)?
                } else {
                    src.generic(w, hc)?
                };
                new.push(Rc::new(bm));
            }
            decoded += 1;
        }
        if decoded == first {
            empty += 1;
            if empty > MAX_EMPTY_CLASSES {
                return Err("符号字典里空的高度类太多".into());
            }
        }
        // 4c：集合位图（§6.5.9）
        if collective {
            src.collective(&widths, hc, &mut new)?;
        }
    }
    Ok(new)
}

/// 细化/聚合编码的符号位图（§6.5.8.2）。`new` 是这本字典里已解出的新符号。
fn refagg(
    src: &mut impl DictSource,
    h: &Header,
    input: &[Rc<Bitmap>],
    new: &[Rc<Bitmap>],
    w: usize,
    hc: usize,
) -> Result<Bitmap, String> {
    let n = src
        .int(DictField::AggInst)?
        .ok_or("REFAGGNINST 不能是 OOB")?;
    let n = u32::try_from(n)
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("REFAGGNINST {n} 无效"))?;
    let syms = Symbols {
        head: input,
        tail: new,
    };
    let t = src.text();
    if n == 1 {
        // §6.5.8.2.2：只有一个实例时直接细化。[协议] 参数按表 18：尺寸是本符号的，偏移是 RDX、RDY，
        // 不用 TPGRON
        let id = t.id()?;
        let mut d = [0i32; 2];
        for (v, f) in d.iter_mut().zip([Field::Rdx, Field::Rdy]) {
            *v = t.int(f)?.ok_or_else(|| format!("{f:?} 不能是 OOB"))?;
        }
        let p = refine::Params {
            w,
            h: hc,
            template: h.rtemplate,
            reference: syms.get(id)?,
            dx: d[0],
            dy: d[1],
            tpgron: false,
            at: h.rat,
        };
        return t.refine(&p);
    }
    // §6.5.8.2.1：多个实例时用文本区域解码过程。
    // [协议] 参数按表 17：一个条带、左上角、不转置、OR 合成、底色 0、SBDSOFFSET 0、要细化
    let p = text::Params {
        w,
        h: hc,
        instances: n,
        log_strips: 0,
        corner: Corner::TopLeft,
        transposed: false,
        op: Op::Or,
        default_pixel: 0,
        ds_offset: 0,
        refine: true,
        rtemplate: h.rtemplate,
        rat: h.rat,
    };
    text::decode(t, &p, &syms)
}

/// 导出符号（§6.5.10）：导出标志按游程交替（先是“不导出”），输入符号在前、新符号在后。
fn export(
    src: &mut impl DictSource,
    input: &[Rc<Bitmap>],
    new: &[Rc<Bitmap>],
) -> Result<Vec<Rc<Bitmap>>, String> {
    let total = input.len() + new.len();
    // [策略] 游程数上限：每个非零游程至少推进 1，正常码流里零游程只出现在开头，放宽到两倍再留余量
    let max_runs = total.saturating_mul(2).saturating_add(2);
    let mut out = Vec::new();
    let (mut i, mut flag, mut runs) = (0usize, false, 0usize);
    while i < total {
        let r = src.int(DictField::Ex)?.ok_or("EXRUNLENGTH 不能是 OOB")?;
        let r = usize::try_from(r).map_err(|_| format!("EXRUNLENGTH {r} 为负"))?;
        let end = i
            .checked_add(r)
            .filter(|&e| e <= total)
            .ok_or("导出标志游程超出了符号总数")?;
        if flag {
            // 导出结果存到解码结束，引用来的符号也算：指针不走 `Bitmap::new`
            bitmap::charge((end - i).saturating_mul(size_of::<Rc<Bitmap>>()))?;
            let n = input.len();
            out.extend((i..end).map(|k| {
                if k < n {
                    input[k].clone()
                } else {
                    new[k - n].clone()
                }
            }));
        }
        i = end;
        flag = !flag;
        runs += 1;
        if runs > max_runs {
            return Err("导出标志游程太多".into());
        }
    }
    Ok(out)
}
