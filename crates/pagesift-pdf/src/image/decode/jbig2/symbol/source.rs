//! 符号字典各字段的两种来源（T.88 §6.5.4 表 13）：算术编码（IADH、IADW、IAAI、IAEX，
//! 通用区域）和霍夫曼编码（§7.4.2.1.6 选表，集合位图 §6.5.9）。细化/聚合时还要提供文本区域的
//! 来源，与这里共用同一个码流和读位位置。

use std::rc::Rc;

use super::super::bitmap::Bitmap;
use super::super::generic;
use super::super::huffman::{self, Table};
use super::super::mq::{Cx, IntDecoder, Mq, STARVED};
use super::super::text::coder::{Arith, ArithCx, Huff, HuffTables, Ids, Picker, Reader, Source};
use super::Header;

/// 符号字典自己的整数字段。
#[derive(Debug, Clone, Copy)]
pub(super) enum DictField {
    /// HCDH：高度类的高度增量（§6.5.6）。
    Dh,
    /// DW：同一高度类里符号的宽度增量，OOB 表示高度类结束（§6.5.7）。
    Dw,
    /// REFAGGNINST：聚合的实例数（§6.5.8.2）。
    AggInst,
    /// EXRUNLENGTH：导出标志的游程（§6.5.10）。
    Ex,
}

pub(super) trait DictSource {
    /// 解一个整数字段；None 是 OOB。
    fn int(&mut self, f: DictField) -> Result<Option<i32>, String>;
    /// 直接编码的符号位图（§6.5.8.1），只在算术编码时有。
    fn generic(&mut self, w: usize, h: usize) -> Result<Bitmap, String>;
    /// 高度类的集合位图（§6.5.9），只在霍夫曼编码且不用 REFAGG 时有：按 `widths` 切开追加到 `out`。
    fn collective(
        &mut self,
        widths: &[usize],
        h: usize,
        out: &mut Vec<Rc<Bitmap>>,
    ) -> Result<(), String>;
    /// 细化/聚合用的文本区域来源（§6.5.8.2）。
    fn text(&mut self) -> &mut dyn Source;
}

/// 算术编码的符号字典：与文本区域来源共用一个 MQ 解码器。
pub(super) struct ArithDict<'a> {
    t: Arith<'a>,
    dh: IntDecoder,
    dw: IntDecoder,
    ai: IntDecoder,
    ex: IntDecoder,
    /// 通用区域的上下文，同一字典里所有符号共用（§6.5.8.1）。
    gb: Vec<Cx>,
    template: u8,
    at: [(i16, i16); 4], // [协议] SDAT 至多 4 个（§7.4.2.1.2）
}

impl<'a> ArithDict<'a> {
    pub(super) fn new(
        h: &Header,
        body: &'a [u8],
        id_len: u32,
        gb: Vec<Cx>,
        gr: Vec<Cx>,
    ) -> Result<ArithDict<'a>, String> {
        Ok(ArithDict {
            t: Arith {
                mq: Mq::new(body),
                cx: ArithCx::new(id_len, gr)?,
            },
            dh: IntDecoder::new(),
            dw: IntDecoder::new(),
            ai: IntDecoder::new(),
            ex: IntDecoder::new(),
            gb,
            template: h.template,
            at: h.at,
        })
    }

    /// 解完后的通用区域和细化上下文（留给后续字典沿用）。
    pub(super) fn into_contexts(self) -> (Vec<Cx>, Vec<Cx>) {
        (self.gb, self.t.cx.gr)
    }
}

impl DictSource for ArithDict<'_> {
    fn int(&mut self, f: DictField) -> Result<Option<i32>, String> {
        let d = match f {
            DictField::Dh => &mut self.dh,
            DictField::Dw => &mut self.dw,
            DictField::AggInst => &mut self.ai,
            DictField::Ex => &mut self.ex,
        };
        if self.t.mq.starved() {
            return Err(STARVED.into());
        }
        Ok(d.decode(&mut self.t.mq))
    }

    fn generic(&mut self, w: usize, h: usize) -> Result<Bitmap, String> {
        // 表 16：MMR 0、TPGDON 0、不用 SKIP，模板和 AT 取自字典
        let p = generic::Params {
            w,
            h,
            template: self.template,
            tpgdon: false,
            at: self.at,
            skip: None,
        };
        generic::decode(&mut self.t.mq, &mut self.gb, &p)
    }

    fn collective(&mut self, _: &[usize], _: usize, _: &mut Vec<Rc<Bitmap>>) -> Result<(), String> {
        Err("算术编码的符号字典没有集合位图".into())
    }

    fn text(&mut self) -> &mut dyn Source {
        &mut self.t
    }
}

/// 霍夫曼编码的符号字典：与文本区域来源共用一个读位器。
pub(super) struct HuffDict<'a, 't> {
    t: Huff<'a, 't>,
    dh: &'t Table,
    dw: &'t Table,
    bmsize: &'t Table,
    agg: &'t Table,
    ex: &'static Table,
}

impl<'a, 't> HuffDict<'a, 't> {
    pub(super) fn new(
        h: &Header,
        body: &'a [u8],
        tables: &'t [Rc<Table>],
        id_len: u32,
        gr: Vec<Cx>,
    ) -> Result<HuffDict<'a, 't>, String> {
        let mut pk = Picker::new(tables);
        // [协议] 自定义表按 DH、DW、BMSIZE、AGGINST 的顺序取（§7.4.2.1.6）；两位选择值 3、
        // 一位选择值 1 是自定义表；SDHUFFDH 0/1 是 B.4/B.5，SDHUFFDW 0/1 是 B.2/B.3，
        // SDHUFFBMSIZE、SDHUFFAGGINST 0 是 B.1（§7.4.2.1.1）
        let dh = pk.pick("SDHUFFDH", h.sel_dh, 3, &[4, 5])?;
        let dw = pk.pick("SDHUFFDW", h.sel_dw, 3, &[2, 3])?; // [协议] 同上，3 是自定义表
        let bmsize = pk.pick("SDHUFFBMSIZE", h.sel_bmsize, 1, &[1])?;
        let agg = pk.pick("SDHUFFAGGINST", h.sel_agginst, 1, &[1])?;
        let text = if h.refagg {
            HuffTables {
                // [协议] 细化/聚合时文本区域用的表（表 17）：FS B.6、DS B.8、DT B.11、RDW/RDH/RDX/RDY B.15、
                // RSIZE B.1；只有一个实例时的 RDX、RDY 用 B.15，BMSIZE 用 B.1（§6.5.8.2.2），与此相同
                fs: huffman::standard(6)?,
                ds: huffman::standard(8)?,
                dt: huffman::standard(11)?,
                rdw: huffman::standard(15)?,
                rdh: huffman::standard(15)?,
                rdx: huffman::standard(15)?,
                rdy: huffman::standard(15)?,
                rsize: huffman::standard(1)?,
            }
        } else {
            // 不用 REFAGG 时文本区域来源不会被调用，任填一张
            HuffTables {
                fs: bmsize,
                ds: bmsize,
                dt: bmsize,
                rdw: bmsize,
                rdh: bmsize,
                rdx: bmsize,
                rdy: bmsize,
                rsize: bmsize,
            }
        };
        Ok(HuffDict {
            t: Huff {
                rd: Reader::new(body),
                t: text,
                // 符号 ID 是 SBSYMCODELEN 位的定长码（§6.5.8.2.3）
                ids: Ids::Fixed(id_len),
                gr,
            },
            dh,
            dw,
            bmsize,
            agg,
            // [协议] EXRUNLENGTH 用表 B.1（§6.5.10）
            ex: huffman::standard(1)?,
        })
    }

    /// 解完后的细化上下文。
    pub(super) fn into_gr(self) -> Vec<Cx> {
        self.t.gr
    }
}

impl DictSource for HuffDict<'_, '_> {
    fn int(&mut self, f: DictField) -> Result<Option<i32>, String> {
        let t = match f {
            DictField::Dh => self.dh,
            DictField::Dw => self.dw,
            DictField::AggInst => self.agg,
            DictField::Ex => self.ex,
        };
        self.t.rd.decode(t)
    }

    fn generic(&mut self, _: usize, _: usize) -> Result<Bitmap, String> {
        Err("霍夫曼编码的符号字典不逐个解符号位图".into())
    }

    fn collective(
        &mut self,
        widths: &[usize],
        h: usize,
        out: &mut Vec<Rc<Bitmap>>,
    ) -> Result<(), String> {
        let rd = &mut self.t.rd;
        // §6.5.9 步骤 1、2：BMSIZE，然后跳过当前字节剩下的位（`take` 先对齐）
        let size = rd.decode(self.bmsize)?.ok_or("BMSIZE 不能是 OOB")?;
        let size = usize::try_from(size).map_err(|_| format!("BMSIZE {size} 为负"))?;
        let total = widths
            .iter()
            .try_fold(0usize, |a, &w| a.checked_add(w))
            .ok_or("高度类总宽度溢出")?;
        let all = if size == 0 {
            // 步骤 3：未压缩，每行 TOTWIDTH 位、补齐到字节，高位在前
            let stride = total.div_ceil(8); // [协议] 每字节 8 像素
            let raw = rd.take(stride.checked_mul(h).ok_or("集合位图尺寸溢出")?);
            let mut bm = Bitmap::new(total, h, 0)?;
            for y in 0..h {
                let row = raw.get(y * stride..).unwrap_or(&[]);
                for (x, px) in bm.row_mut(y).iter_mut().enumerate() {
                    // [协议] 字节内高位在前（§6.5.9 步骤 3）；数据不足的部分当白
                    let byte = row.get(x / 8).copied().unwrap_or(0);
                    *px = (byte >> (7 - x % 8)) & 1; // [协议] 第 x % 8 位，最高位是 7
                }
            }
            bm
        } else {
            // 步骤 4：MMR 编码的通用区域，正好占 BMSIZE 字节
            generic::mmr(rd.take(size), total, h)?.0
        };
        // 步骤 5：按各符号宽度切开
        let mut x = 0usize;
        for &w in widths {
            out.push(Rc::new(all.crop(x as i64, 0, w, h)?));
            x += w;
        }
        Ok(())
    }

    fn text(&mut self) -> &mut dyn Source {
        &mut self.t
    }
}
