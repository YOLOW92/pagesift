//! 文本区域解码过程（T.88 §6.4）和文本区域段（§7.4.3）。
//!
//! 解码过程 [`decode`] 按参数（表 9）工作，字段从 [`coder::Source`] 读：文本区域段和符号字典的
//! 细化/聚合编码（§6.5.8.2）共用它。

pub(super) mod coder;
#[cfg(test)]
pub(super) mod tests;

use std::borrow::Cow;
use std::rc::Rc;

use super::RegionInfo;
use super::bitmap::{Bitmap, Op};
use super::huffman::{self, Table};
use super::mq::{Mq, contexts};
use super::refine;
use coder::{Arith, ArithCx, Field, Huff, HuffTables, Ids, Picker, Reader, Source};

/// REFCORNER：符号实例的哪个角落在 (S, T) 上（§6.4.5 步骤 3c vi）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Corner {
    BottomLeft,
    TopLeft,
    BottomRight,
    TopRight,
}

impl Corner {
    fn from_bits(v: u16) -> Corner {
        // [协议] 0 BOTTOMLEFT、1 TOPLEFT、2 BOTTOMRIGHT、3 TOPRIGHT（§7.4.3.1.1 位 4–5）
        match v & 3 {
            0 => Corner::BottomLeft,
            1 => Corner::TopLeft,
            2 => Corner::BottomRight,
            _ => Corner::TopRight,
        }
    }

    fn left(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::BottomLeft)
    }

    fn top(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::TopRight)
    }
}

/// 文本区域解码过程的参数（表 9；SBHUFF、SBSYMS 等由 [`Source`] 和 [`Symbols`] 体现）。
pub(super) struct Params {
    /// SBW、SBH。
    pub(super) w: usize,
    pub(super) h: usize,
    /// SBNUMINSTANCES。
    pub(super) instances: u32,
    /// LOG2SBSTRIPS（SBSTRIPS = 2^它）。
    pub(super) log_strips: u32,
    pub(super) corner: Corner,
    pub(super) transposed: bool,
    /// SBCOMBOP。
    pub(super) op: Op,
    /// SBDEFPIXEL。
    pub(super) default_pixel: u8,
    /// SBDSOFFSET。
    pub(super) ds_offset: i32,
    /// SBREFINE、SBRTEMPLATE、SBRAT。
    pub(super) refine: bool,
    pub(super) rtemplate: u8,
    pub(super) rat: [(i8, i8); 2],
}

/// SBSYMS：符号按 ID 排成一列。符号字典的聚合里是输入符号接着已解出的新符号，所以分两段。
pub(super) struct Symbols<'s> {
    pub(super) head: &'s [Rc<Bitmap>],
    pub(super) tail: &'s [Rc<Bitmap>],
}

impl Symbols<'_> {
    pub(super) fn get(&self, id: usize) -> Result<&Bitmap, String> {
        let n = self.head.len();
        let s = if id < n {
            self.head.get(id)
        } else {
            self.tail.get(id - n)
        };
        s.map(|b| &**b)
            .ok_or_else(|| format!("符号 ID {id} 超出符号数 {}", n + self.tail.len()))
    }
}

/// 文本区域解码过程（§6.4.5）。
pub(super) fn decode(src: &mut dyn Source, p: &Params, syms: &Symbols) -> Result<Bitmap, String> {
    let mut out = Bitmap::new(p.w, p.h, p.default_pixel)?;
    // [协议] SBSTRIPS = 2^LOG2SBSTRIPS（§7.4.3.1.1 位 2–3）
    let strips = 1i64 << p.log_strips;
    // STRIPT 的增量要乘 SBSTRIPS（§6.4.6）
    let dt = |src: &mut dyn Source| -> Result<i64, String> {
        let v = src.int(Field::Dt)?.ok_or("DT 不能是 OOB")?;
        Ok(i64::from(v) * strips)
    };
    // 步骤 2：初始 STRIPT 取负
    let mut strip_t = -dt(src)?;
    let mut first_s = 0i64;
    let total = u64::from(p.instances);
    let mut n = 0u64;
    // 步骤 3。坐标累加用饱和加法：[策略] 坏码流可累加 2^32 次 32 位增量，i64 也可能溢出
    'strips: while n < total {
        strip_t = strip_t.saturating_add(dt(src)?);
        // 3c i：条带里第一个实例（§6.4.7）
        let dfs = src.int(Field::Fs)?.ok_or("DFS 不能是 OOB")?;
        first_s = first_s.saturating_add(i64::from(dfs));
        let mut cur_s = first_s;
        loop {
            // 实例数和符号大小相乘没有上限（分配上限只管住了区域本身）
            super::bitmap::check_time()?;
            // 3c iii：T 坐标（§6.4.9）
            let cur_t = if p.log_strips == 0 {
                0
            } else {
                src.curt(p.log_strips)?
            };
            let t = strip_t.saturating_add(i64::from(cur_t));
            // 3c iv、v：符号 ID 和实例位图（§6.4.10、§6.4.11）
            let id = src.id()?;
            let ib = instance(src, p, syms.get(id)?)?;
            let (wi, hi) = (ib.w as i64, ib.h as i64);
            // 3c vi：放置前按参考角推进 CURS
            if !p.transposed && !p.corner.left() {
                cur_s += wi - 1;
            } else if p.transposed && !p.corner.top() {
                cur_s += hi - 1;
            }
            let (px, py) = if p.transposed { (t, cur_s) } else { (cur_s, t) };
            let x = if p.corner.left() { px } else { px - wi + 1 };
            let y = if p.corner.top() { py } else { py - hi + 1 };
            // 3c ix：按 SBCOMBOP 画上
            out.compose(&ib, x, y, p.op);
            // 3c x：放置后推进 CURS
            if !p.transposed && p.corner.left() {
                cur_s += wi - 1;
            } else if p.transposed && p.corner.top() {
                cur_s += hi - 1;
            }
            n += 1;
            // 3c ii：后续实例的 S 坐标，OOB 时条带结束（§6.4.8）
            match src.int(Field::Ds)? {
                None => break,
                // [策略] 实例已够 SBNUMINSTANCES 却没有 OOB：按规范这是坏码流，就此停下，
                // 防止读过数据末尾后无止境地解下去
                Some(_) if n >= total => break 'strips,
                Some(ds) => cur_s = cur_s.saturating_add(i64::from(ds) + i64::from(p.ds_offset)),
            }
        }
    }
    Ok(out)
}

/// 符号实例位图 IBI（§6.4.11）：不细化时就是符号本身，细化时按 §6.4.11.1 解出新位图。
fn instance<'s>(
    src: &mut dyn Source,
    p: &Params,
    sym: &'s Bitmap,
) -> Result<Cow<'s, Bitmap>, String> {
    if !p.refine || !src.ri()? {
        return Ok(Cow::Borrowed(sym));
    }
    let mut d = [0i64; 4]; // [协议] RDW、RDH、RDX、RDY 四个字段（§6.4.11.1）
    for (v, f) in d
        .iter_mut()
        .zip([Field::Rdw, Field::Rdh, Field::Rdx, Field::Rdy])
    {
        *v = i64::from(src.int(f)?.ok_or_else(|| format!("{f:?} 不能是 OOB"))?);
    }
    let [rdw, rdh, rdx, rdy] = d;
    let size = |base: usize, delta: i64| {
        usize::try_from(base as i64 + delta).map_err(|_| "细化后的符号尺寸为负".to_string())
    };
    // GRREFERENCEDX = ⌊RDW/2⌋ + RDX，GRREFERENCEDY 同理（§6.4.11.1 步骤 3）
    let offset = |rd: i64, r: i64| {
        i32::try_from(rd.div_euclid(2) + r).map_err(|_| "细化偏移溢出".to_string())
    };
    let rp = refine::Params {
        w: size(sym.w, rdw)?,
        h: size(sym.h, rdh)?,
        template: p.rtemplate,
        reference: sym,
        dx: offset(rdw, rdx)?,
        dy: offset(rdh, rdy)?,
        tpgron: false,
        at: p.rat,
    };
    Ok(Cow::Owned(src.refine(&rp)?))
}

/// SBDSOFFSET：标志位 10–14 的 5 位二进制补码（§7.4.3.1.1）。
fn ds_offset(flags: u16) -> i32 {
    // [协议] 位 10–14
    let v = ((flags >> 10) & 0x1F) as u8;
    // [推导] 左移 3 位（8 − 5）把符号位放到 i8 的最高位，再算术右移回来
    i32::from(((v << 3) as i8) >> 3)
}

fn be16(b: &[u8], i: usize) -> Result<u16, String> {
    let s = b.get(i..i + 2).ok_or("文本区域段太短")?; // [协议] 2 字节大端整数
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

/// 文本区域段（类型 4、6、7）区域信息之后的数据（§7.4.3.1）。`symbols` 是引用的各符号字典
/// 导出的符号（SBSYMS，按引用顺序拼接），`tables` 是引用的码表段。
pub(super) fn region(
    data: &[u8],
    info: &RegionInfo,
    symbols: &[Rc<Bitmap>],
    tables: &[Rc<Table>],
) -> Result<Bitmap, String> {
    // [协议] 文本区域段标志 2 字节，各字段的位置见每行（§7.4.3.1.1）
    let flags = be16(data, 0)?;
    let huff = flags & 1 != 0; // [协议] 位 0 SBHUFF
    let refine = flags & 2 != 0; // [协议] 位 1 SBREFINE
    let log_strips = u32::from((flags >> 2) & 3); // [协议] 位 2–3 LOG2SBSTRIPS
    let corner = Corner::from_bits(flags >> 4); // [协议] 位 4–5 REFCORNER
    let transposed = flags & 0x40 != 0; // [协议] 位 6 TRANSPOSED
    let op = Op::from_bits(((flags >> 7) & 3) as u8)?; // [协议] 位 7–8 SBCOMBOP
    let default_pixel = ((flags >> 9) & 1) as u8; // [协议] 位 9 SBDEFPIXEL
    let ds_offset = ds_offset(flags); // [协议] 位 10–14 SBDSOFFSET
    let rtemplate = (flags >> 15) as u8; // [协议] 位 15 SBRTEMPLATE
    let mut pos = 2; // [协议] 标志之后
    let hflags = if huff {
        let v = be16(data, pos)?;
        pos += 2; // [协议] 霍夫曼标志 2 字节（§7.4.3.1.2）
        v
    } else {
        0
    };
    let mut rat = [(0i8, 0i8); 2];
    if refine && rtemplate == 0 {
        // [协议] 细化 AT 像素 4 字节：SBRATX1、SBRATY1、SBRATX2、SBRATY2（§7.4.3.1.3）
        let b = data
            .get(pos..pos + 4)
            .ok_or("文本区域段的细化 AT 像素不全")?;
        rat = [(b[0] as i8, b[1] as i8), (b[2] as i8, b[3] as i8)];
        pos += 4; // [协议] 细化 AT 像素 4 字节
    }
    let instances = super::be32(data, pos)?;
    pos += 4; // [协议] SBNUMINSTANCES 4 字节（§7.4.3.1.4）
    let rest = data.get(pos..).ok_or("文本区域段太短")?;
    let p = Params {
        w: info.w,
        h: info.h,
        instances,
        log_strips,
        corner,
        transposed,
        op,
        default_pixel,
        ds_offset,
        refine,
        rtemplate,
        rat,
    };
    let syms = Symbols {
        head: symbols,
        tail: &[],
    };
    let gr = if refine {
        contexts(refine::context_count(rtemplate))
    } else {
        Vec::new()
    };
    if huff {
        let t = huff_tables(hflags, tables)?;
        let mut rd = Reader::new(rest);
        // 符号 ID 码表在段数据里，读完对齐字节（§7.4.3.1.7）
        let ids = Ids::Table(Box::new(huffman::symbol_ids(rd.bits_mut(), symbols.len())?));
        let mut src = Huff { rd, t, ids, gr };
        decode(&mut src, &p, &syms)
    } else {
        let cx = ArithCx::new(coder::code_len(symbols.len()), gr)?;
        let mut src = Arith {
            mq: Mq::new(rest),
            cx,
        };
        decode(&mut src, &p, &syms)
    }
}

/// 按霍夫曼标志选各字段的表（§7.4.3.1.2、§7.4.3.1.6：自定义表按 FS、DS、DT、RDW、RDH、RDX、RDY、
/// RSIZE 的顺序取自引用的码表段）。
fn huff_tables<'t>(f: u16, tables: &'t [Rc<Table>]) -> Result<HuffTables<'t>, String> {
    let mut pk = Picker::new(tables);
    // [协议] 两位的选择值 3、一位的选择值 1 表示自定义表（§7.4.3.1.2）
    const C2: u16 = 3;
    Ok(HuffTables {
        // [协议] 各字段的位置和选择值对应的标准表（§7.4.3.1.2）
        fs: pk.pick("SBHUFFFS", f & 3, C2, &[6, 7])?,
        ds: pk.pick("SBHUFFDS", (f >> 2) & 3, C2, &[8, 9, 10])?,
        dt: pk.pick("SBHUFFDT", (f >> 4) & 3, C2, &[11, 12, 13])?,
        rdw: pk.pick("SBHUFFRDW", (f >> 6) & 3, C2, &[14, 15])?,
        rdh: pk.pick("SBHUFFRDH", (f >> 8) & 3, C2, &[14, 15])?,
        rdx: pk.pick("SBHUFFRDX", (f >> 10) & 3, C2, &[14, 15])?,
        rdy: pk.pick("SBHUFFRDY", (f >> 12) & 3, C2, &[14, 15])?,
        rsize: pk.pick("SBHUFFRSIZE", (f >> 14) & 1, 1, &[1])?,
    })
}
