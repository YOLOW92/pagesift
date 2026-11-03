//! 文本区域各字段的两种来源（T.88 §6.4.4 表 9）：算术编码（IAx、IAID，附录 A）和霍夫曼编码。
//! 符号字典的细化/聚合编码（§6.5.8.2）也通过这里调用文本区域解码过程，所以接口对 jbig2 模块可见。

use std::rc::Rc;

use super::super::bitmap::Bitmap;
use super::super::huffman::{self, Bits, Table};
use super::super::mq::{Cx, IdDecoder, IntDecoder, Mq, STARVED};
use super::super::refine;

/// 文本区域里用整数解码过程解的字段（表 9）。
#[derive(Debug, Clone, Copy)]
pub(in crate::image::decode::jbig2) enum Field {
    /// DT：条带 T 坐标的增量（§6.4.6）。
    Dt,
    /// DFS：条带里第一个实例的 S 坐标增量（§6.4.7）。
    Fs,
    /// IDS：同一条带里后续实例的 S 坐标增量，OOB 表示条带结束（§6.4.8）。
    Ds,
    /// RDW、RDH、RDX、RDY：细化实例的尺寸和位置增量（§6.4.11.1）。
    Rdw,
    Rdh,
    Rdx,
    Rdy,
}

/// 文本区域解码过程读字段的接口：算术与霍夫曼两种实现。
pub(in crate::image::decode::jbig2) trait Source {
    /// 解一个整数字段；None 是 OOB。
    fn int(&mut self, f: Field) -> Result<Option<i32>, String>;
    /// CURT（§6.4.9）：算术编码用 IAIT，霍夫曼编码直接读 LOG2SBSTRIPS 位。
    fn curt(&mut self, log_strips: u32) -> Result<i32, String>;
    /// 符号 ID（§6.4.10）。
    fn id(&mut self) -> Result<usize, String>;
    /// RI（§6.4.11）：算术编码用 IARI，霍夫曼编码读 1 位。
    fn ri(&mut self) -> Result<bool, String>;
    /// 细化出实例位图（§6.4.11.1 步骤 3）。霍夫曼编码时先读 BMSIZE、对齐字节，细化数据是其后
    /// BMSIZE 字节的算术编码码流，读完跳到它之后（步骤 2、4）。
    fn refine(&mut self, p: &refine::Params) -> Result<Bitmap, String>;
}

fn oob(what: &str) -> String {
    format!("{what} 不能是 OOB")
}

/// 算术编码的各组上下文：文本区域段里每段新建一组；符号字典的细化/聚合在整本字典里共用一组
/// （§6.5.5 步骤 1 之后对这些上下文的初始化只做一次）。
pub(in crate::image::decode::jbig2) struct ArithCx {
    dt: IntDecoder,
    fs: IntDecoder,
    ds: IntDecoder,
    it: IntDecoder,
    ri: IntDecoder,
    rdw: IntDecoder,
    rdh: IntDecoder,
    rdx: IntDecoder,
    rdy: IntDecoder,
    id: IdDecoder,
    /// 通用细化区域的上下文（所有细化实例共用）。
    pub(in crate::image::decode::jbig2) gr: Vec<Cx>,
}

impl ArithCx {
    /// `code_len` 是 SBSYMCODELEN，`gr` 是细化上下文（不细化时可为空）。
    pub(in crate::image::decode::jbig2) fn new(
        code_len: u32,
        gr: Vec<Cx>,
    ) -> Result<ArithCx, String> {
        Ok(ArithCx {
            dt: IntDecoder::new(),
            fs: IntDecoder::new(),
            ds: IntDecoder::new(),
            it: IntDecoder::new(),
            ri: IntDecoder::new(),
            rdw: IntDecoder::new(),
            rdh: IntDecoder::new(),
            rdx: IntDecoder::new(),
            rdy: IntDecoder::new(),
            id: IdDecoder::new(code_len)?,
            gr,
        })
    }
}

/// 算术编码的来源：MQ 解码器和上下文。
pub(in crate::image::decode::jbig2) struct Arith<'a> {
    pub(in crate::image::decode::jbig2) mq: Mq<'a>,
    pub(in crate::image::decode::jbig2) cx: ArithCx,
}

impl Source for Arith<'_> {
    fn int(&mut self, f: Field) -> Result<Option<i32>, String> {
        // 每个实例都要读整数字段，在这里查一次就覆盖了按计数循环的文本区域和符号字典聚合
        if self.mq.starved() {
            return Err(STARVED.into());
        }
        let c = &mut self.cx;
        let d = match f {
            Field::Dt => &mut c.dt,
            Field::Fs => &mut c.fs,
            Field::Ds => &mut c.ds,
            Field::Rdw => &mut c.rdw,
            Field::Rdh => &mut c.rdh,
            Field::Rdx => &mut c.rdx,
            Field::Rdy => &mut c.rdy,
        };
        Ok(d.decode(&mut self.mq))
    }

    fn curt(&mut self, _log_strips: u32) -> Result<i32, String> {
        self.cx.it.decode(&mut self.mq).ok_or_else(|| oob("CURT"))
    }

    fn id(&mut self) -> Result<usize, String> {
        Ok(self.cx.id.decode(&mut self.mq) as usize)
    }

    fn ri(&mut self) -> Result<bool, String> {
        let v = self.cx.ri.decode(&mut self.mq).ok_or_else(|| oob("RI"))?;
        Ok(v != 0)
    }

    fn refine(&mut self, p: &refine::Params) -> Result<Bitmap, String> {
        refine::decode(&mut self.mq, &mut self.cx.gr, p)
    }
}

/// 高位在前的读位器，能对齐后整块取字节再接着读（霍夫曼编码的细化数据、集合位图要用）。
pub(in crate::image::decode::jbig2) struct Reader<'a> {
    data: &'a [u8],
    /// `bits` 起点在 `data` 里的字节下标。
    base: usize,
    bits: Bits<'a>,
}

impl<'a> Reader<'a> {
    pub(in crate::image::decode::jbig2) fn new(data: &'a [u8]) -> Reader<'a> {
        Reader {
            data,
            base: 0,
            bits: Bits::new(data),
        }
    }

    pub(in crate::image::decode::jbig2) fn bits(&mut self, n: u32) -> Result<u32, String> {
        self.bits.bits(n)
    }

    pub(in crate::image::decode::jbig2) fn bits_mut(&mut self) -> &mut Bits<'a> {
        &mut self.bits
    }

    /// 用表 `t` 解一个值；None 是 OOB。
    pub(in crate::image::decode::jbig2) fn decode(
        &mut self,
        t: &Table,
    ) -> Result<Option<i32>, String> {
        t.decode(&mut self.bits)
    }

    /// 跳过当前字节剩下的位，取接下来的 `n` 字节，读位位置移到它们之后。
    /// [策略] 数据不足 `n` 字节时取到末尾为止：截断的码流常见，能解的部分照样解
    pub(in crate::image::decode::jbig2) fn take(&mut self, n: usize) -> &'a [u8] {
        self.bits.align();
        let start = (self.base + self.bits.byte_pos()).min(self.data.len());
        let end = start.saturating_add(n).min(self.data.len());
        self.base = end;
        self.bits = Bits::new(&self.data[end..]);
        &self.data[start..end]
    }
}

/// 霍夫曼编码时文本区域各字段用的表（§7.4.3.1.6 选定；符号字典里按表 17 固定）。
pub(in crate::image::decode::jbig2) struct HuffTables<'t> {
    pub(in crate::image::decode::jbig2) fs: &'t Table,
    pub(in crate::image::decode::jbig2) ds: &'t Table,
    pub(in crate::image::decode::jbig2) dt: &'t Table,
    pub(in crate::image::decode::jbig2) rdw: &'t Table,
    pub(in crate::image::decode::jbig2) rdh: &'t Table,
    pub(in crate::image::decode::jbig2) rdx: &'t Table,
    pub(in crate::image::decode::jbig2) rdy: &'t Table,
    pub(in crate::image::decode::jbig2) rsize: &'t Table,
}

/// 霍夫曼编码时符号 ID 的码：文本区域段里是段内给出的码表（§7.4.3.1.7），
/// 符号字典的聚合里是 SBSYMCODELEN 位的定长码（§6.5.8.2.3）。
pub(in crate::image::decode::jbig2) enum Ids {
    Table(Box<Table>),
    Fixed(u32),
}

/// 霍夫曼编码的来源。
pub(in crate::image::decode::jbig2) struct Huff<'a, 't> {
    pub(in crate::image::decode::jbig2) rd: Reader<'a>,
    pub(in crate::image::decode::jbig2) t: HuffTables<'t>,
    pub(in crate::image::decode::jbig2) ids: Ids,
    /// 细化数据虽在霍夫曼码流里，本身仍是算术编码，各实例共用这组上下文。
    pub(in crate::image::decode::jbig2) gr: Vec<Cx>,
}

impl Source for Huff<'_, '_> {
    fn int(&mut self, f: Field) -> Result<Option<i32>, String> {
        let t = match f {
            Field::Dt => self.t.dt,
            Field::Fs => self.t.fs,
            Field::Ds => self.t.ds,
            Field::Rdw => self.t.rdw,
            Field::Rdh => self.t.rdh,
            Field::Rdx => self.t.rdx,
            Field::Rdy => self.t.rdy,
        };
        self.rd.decode(t)
    }

    fn curt(&mut self, log_strips: u32) -> Result<i32, String> {
        // 至多 3 位，转 i32 不会溢出
        Ok(self.rd.bits(log_strips)? as i32)
    }

    fn id(&mut self) -> Result<usize, String> {
        let v = match &self.ids {
            Ids::Table(t) => {
                let v = self.rd.decode(t)?.ok_or_else(|| oob("符号 ID"))?;
                usize::try_from(v).map_err(|_| format!("符号 ID {v} 为负"))?
            }
            Ids::Fixed(len) => self.rd.bits(*len)? as usize,
        };
        Ok(v)
    }

    fn ri(&mut self) -> Result<bool, String> {
        // [协议] 霍夫曼编码时 RI 是 1 位（§6.4.11）
        Ok(self.rd.bits(1)? != 0)
    }

    fn refine(&mut self, p: &refine::Params) -> Result<Bitmap, String> {
        let size = self.rd.decode(self.t.rsize)?.ok_or_else(|| oob("BMSIZE"))?;
        let size = usize::try_from(size).map_err(|_| format!("BMSIZE {size} 为负"))?;
        let body = self.rd.take(size);
        let mut mq = Mq::new(body);
        refine::decode(&mut mq, &mut self.gr, p)
    }
}

/// 霍夫曼表的选择：标志值对应的标准表，或按顺序取引用的下一张自定义表（§7.4.2.1.6、§7.4.3.1.6）。
pub(in crate::image::decode::jbig2) struct Picker<'t> {
    custom: std::slice::Iter<'t, Rc<Table>>,
}

impl<'t> Picker<'t> {
    pub(in crate::image::decode::jbig2) fn new(tables: &'t [Rc<Table>]) -> Picker<'t> {
        Picker {
            custom: tables.iter(),
        }
    }

    /// `sel` 是标志里的选择值：等于 `custom` 时取下一张自定义表，否则 `standard[sel]` 是标准表号。
    pub(in crate::image::decode::jbig2) fn pick(
        &mut self,
        what: &str,
        sel: u16,
        custom: u16,
        standard: &[u8],
    ) -> Result<&'t Table, String> {
        if sel == custom {
            return self
                .custom
                .next()
                .map(|t| &**t)
                .ok_or_else(|| format!("{what} 要用自定义码表，但引用的码表段不够"));
        }
        let n = standard
            .get(usize::from(sel))
            .ok_or_else(|| format!("{what} 的表选择值 {sel} 无效"))?;
        huffman::standard(*n)
    }
}

/// SBSYMCODELEN：⌈log2(符号数)⌉（§7.4.3.1.7、§6.5.8.2.3）；符号数 0、1 时为 0。
pub(in crate::image::decode::jbig2) fn code_len(n: usize) -> u32 {
    let mut len = 0u32;
    // [推导] 2^len ≥ n 的最小 len；usize 至多 64 位，不会越过 u128
    while (1u128 << len) < n as u128 {
        len += 1;
    }
    len
}
