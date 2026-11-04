//! JBIG2 解码（ITU-T T.88），即 PDF 的 JBIG2Decode（ISO 32000-2 §7.4.7）。输出 8 位灰度：
//! 黑 0、白 255（PDF 规定 JBIG2 的 1 是黑，滤镜输出按 DeviceGray 取反）。
//!
//! 只解第一页：PDF 嵌入的码流只有一页，全局段（/JBIG2Globals）先处理。各段依次处理：
//! 字典类的段（符号字典、图样字典、码表）把结果按段号存起来，区域段解出位图后合成到页上
//! （中间区域段存起来，留给引用它的细化区域）。

mod bitmap;
mod generic;
mod halftone;
mod huffman;
mod mq;
mod refine;
mod segment;
mod symbol;
#[cfg(test)]
mod tests;
mod text;

use std::collections::HashMap;
use std::rc::Rc;

use super::Raster;
use bitmap::{Bitmap, Op};
use segment::Segment;

/// 区域段信息（T.88 §7.4.1）：区域的大小、在页上的位置和外部合成运算。
#[derive(Debug, Clone)]
struct RegionInfo {
    w: usize,
    h: usize,
    x: i64,
    y: i64,
    op: Op,
}

/// [协议] 区域段信息的长度：宽、高、x、y 各 4 字节，标志 1 字节（§7.4.1）
const REGION_INFO: usize = 17;

fn be32(b: &[u8], i: usize) -> Result<u32, String> {
    let s = b.get(i..i + 4).ok_or("段数据太短")?; // [协议] 4 字节大端整数
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// 读区域段信息，返回它和其后的数据。
fn region_info(data: &[u8]) -> Result<(RegionInfo, &[u8]), String> {
    if data.len() < REGION_INFO {
        return Err("区域段信息不全".into());
    }
    let info = RegionInfo {
        w: be32(data, 0)? as usize,
        h: be32(data, 4)? as usize, // [协议] 区域段信息布局（§7.4.1）
        x: i64::from(be32(data, 8)?),
        y: i64::from(be32(data, 12)?), // [协议] 区域段信息布局（§7.4.1）
        // [协议] 标志的低 3 位是外部合成运算（§7.4.1.5）
        op: Op::from_bits(data[16] & 7)?,
    };
    Ok((info, &data[REGION_INFO..]))
}

/// 存起来留给后面的段引用的结果。
enum Stored {
    Symbols(Vec<Rc<Bitmap>>, Option<Rc<symbol::Retained>>),
    Patterns(Vec<Bitmap>),
    Table(Rc<huffman::Table>),
    Region(Bitmap),
}

struct Page {
    bitmap: Bitmap,
    /// 页高未知（0xFFFFFFFF）：随区域和条带尾段增长（§7.4.8.2）。
    grows: bool,
    default: u8,
}

#[derive(Default)]
struct State {
    page: Option<Page>,
    stored: HashMap<u32, Stored>,
}

/// 处理完一段之后：继续，还是这一页已经结束。
#[derive(PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

/// [协议] 段类型（T.88 §7.3 表 2）
mod kind {
    pub(super) const SYMBOL_DICT: u8 = 0;
    pub(super) const TEXT: [u8; 3] = [4, 6, 7]; // [协议] 中间、立即、立即无损文本区域
    pub(super) const PATTERN_DICT: u8 = 16; // [协议] 图样字典
    pub(super) const HALFTONE: [u8; 3] = [20, 22, 23]; // [协议] 中间、立即、立即无损半色调区域
    pub(super) const GENERIC: [u8; 3] = [36, 38, 39]; // [协议] 中间、立即、立即无损通用区域
    pub(super) const REFINE: [u8; 3] = [40, 42, 43]; // [协议] 中间、立即、立即无损细化区域
    /// [协议] 中间区域段（文本、半色调、通用、细化四种）：结果不上页，留给细化区域引用。
    pub(super) const INTERMEDIATE: [u8; 4] = [4, 20, 36, 40];
    pub(super) const PAGE_INFO: u8 = 48; // [协议] 页信息
    pub(super) const END_OF_PAGE: u8 = 49; // [协议] 页尾
    pub(super) const END_OF_STRIPE: u8 = 50; // [协议] 条带尾
    pub(super) const END_OF_FILE: u8 = 51; // [协议] 文件尾
    pub(super) const TABLES: u8 = 53; // [协议] 码表
}

impl State {
    /// 引用的段里某一类的结果，按引用顺序。
    fn referred<'s, T>(&'s self, seg: &Segment, pick: impl Fn(&'s Stored) -> Option<T>) -> Vec<T> {
        seg.refs
            .iter()
            .filter_map(|r| self.stored.get(r).and_then(&pick))
            .collect()
    }

    fn tables(&self, seg: &Segment) -> Vec<Rc<huffman::Table>> {
        self.referred(seg, |s| match s {
            Stored::Table(t) => Some(t.clone()),
            _ => None,
        })
    }

    /// 引用的各符号字典导出的符号，按引用顺序拼起来。同一本字典可以被引用很多次（每次只占 1 字节），
    /// 这里全是 `Rc` 克隆，不走 `Bitmap::new`；不计费的话几层互相引用的字典就能要上百 GB 指针。
    fn referred_symbols(&self, seg: &Segment) -> Result<Vec<Rc<Bitmap>>, String> {
        let lists = self.referred(seg, |s| match s {
            Stored::Symbols(v, _) => Some(v),
            _ => None,
        });
        let n = lists
            .iter()
            .try_fold(0usize, |n, v| n.checked_add(v.len()))
            .and_then(|n| n.checked_mul(size_of::<Rc<Bitmap>>()))
            .ok_or("符号数溢出")?;
        bitmap::charge(n)?;
        Ok(lists.into_iter().flatten().cloned().collect())
    }

    fn process(&mut self, seg: &Segment) -> Result<Flow, String> {
        bitmap::check_time()?;
        let k = seg.kind;
        match k {
            kind::PAGE_INFO => self.page_info(seg.data)?,
            kind::END_OF_PAGE | kind::END_OF_FILE => return Ok(Flow::Stop),
            kind::END_OF_STRIPE => {
                // [协议] 条带尾段：4 字节，本条带最后一行的行号（§7.4.10）
                let last = be32(seg.data, 0)? as usize;
                if let Some(p) = self.page.as_mut().filter(|p| p.grows) {
                    p.bitmap.grow(last + 1, p.default)?;
                }
            }
            kind::TABLES => {
                let t = huffman::custom(seg.data)?;
                self.stored.insert(seg.number, Stored::Table(Rc::new(t)));
            }
            kind::SYMBOL_DICT => {
                let input = self.referred_symbols(seg)?;
                // 沿用上下文时取引用的最后一个符号字典留下的（§7.4.2.2 步骤 3）
                let prior = self
                    .referred(seg, |s| match s {
                        Stored::Symbols(_, r) => Some(r.clone()),
                        _ => None,
                    })
                    .pop()
                    .flatten();
                let tables = self.tables(seg);
                let (out, retained) =
                    symbol::dictionary(seg.data, &input, &tables, prior.as_deref())?;
                let retained = retained.map(Rc::new);
                self.stored
                    .insert(seg.number, Stored::Symbols(out, retained));
            }
            kind::PATTERN_DICT => {
                let p = halftone::pattern_dict(seg.data)?;
                self.stored.insert(seg.number, Stored::Patterns(p));
            }
            _ if kind::TEXT.contains(&k)
                || kind::HALFTONE.contains(&k)
                || kind::GENERIC.contains(&k)
                || kind::REFINE.contains(&k) =>
            {
                self.region(seg)?
            }
            _ => {} // 配置文件段、扩展段等：不影响像素
        }
        Ok(Flow::Go)
    }

    /// 页信息段（§7.4.8）。
    fn page_info(&mut self, data: &[u8]) -> Result<(), String> {
        if self.page.is_some() {
            return Err("出现了第二个页信息段".into());
        }
        let w = be32(data, 0)? as usize;
        let h = be32(data, 4)?; // [协议] 页高在第 4 字节起（§7.4.8.2）
        // [协议] 宽、高、x 分辨率、y 分辨率各 4 字节，然后 1 字节标志；位 2 是缺省像素值（§7.4.8.5）
        let flags = *data.get(16).ok_or("页信息段太短")?;
        let default = (flags >> 2) & 1;
        let grows = h == u32::MAX;
        let h = if grows { 0 } else { h as usize };
        self.page = Some(Page {
            bitmap: Bitmap::new(w, h, default)?,
            grows,
            default,
        });
        Ok(())
    }

    /// 区域段：解出位图，中间区域存起来，其余合成到页上。
    fn region(&mut self, seg: &Segment) -> Result<(), String> {
        let (mut info, body) = region_info(seg.data)?;
        let k = seg.kind;
        // [协议] 高度 0xFFFFFFFF 的立即通用区域：实际行数在段数据最后 4 字节（§7.4.6.4）
        if kind::GENERIC.contains(&k) && info.h == u32::MAX as usize {
            let n = seg.data.len().checked_sub(4).ok_or("通用区域段太短")?; // [协议] 最后 4 字节
            info.h = be32(seg.data, n)? as usize;
        }
        let bm = if kind::GENERIC.contains(&k) {
            generic::segment(body, &info)?
        } else if kind::TEXT.contains(&k) {
            let symbols = self.referred_symbols(seg)?;
            text::region(body, &info, &symbols, &self.tables(seg))?
        } else if kind::HALFTONE.contains(&k) {
            let patterns = self
                .referred(seg, |s| match s {
                    Stored::Patterns(p) => Some(p),
                    _ => None,
                })
                .into_iter()
                .next()
                .ok_or("半色调区域没有引用图样字典")?;
            halftone::region(body, &info, patterns)?
        } else {
            // 细化区域：参照图是引用的中间区域，没有引用时是页上同一位置的内容（§7.4.7.5）
            let referred = self
                .referred(seg, |s| match s {
                    Stored::Region(b) => Some(b),
                    _ => None,
                })
                .into_iter()
                .next()
                .cloned();
            let reference = match referred {
                Some(b) => b,
                None => {
                    let p = self.page.as_ref().ok_or("细化区域出现在页信息段之前")?;
                    p.bitmap.crop(info.x, info.y, info.w, info.h)?
                }
            };
            refine::segment(body, &info, &reference)?
        };
        if kind::INTERMEDIATE.contains(&k) {
            self.stored.insert(seg.number, Stored::Region(bm));
            return Ok(());
        }
        let p = self.page.as_mut().ok_or("区域段出现在页信息段之前")?;
        if p.grows {
            let bottom = usize::try_from(info.y)
                .ok()
                .and_then(|y| y.checked_add(bm.h))
                .ok_or("区域位置溢出")?;
            p.bitmap.grow(bottom, p.default)?;
        }
        p.bitmap.compose(&bm, info.x, info.y, info.op);
        Ok(())
    }
}

/// 解一段 JBIG2 码流（`globals` 是 /JBIG2Globals 流）。`max_bytes` 是解码过程中分配的位图
/// 总字节数上限（防解压炸弹），PDF 里沿用 `Options::max_stream_bytes`。中途某段出错时保留已经
/// 合成到页上的内容（受损的码流常见，能读的部分仍有用）；没有页可交时才报错。
pub(crate) fn decode(
    data: &[u8],
    globals: Option<&[u8]>,
    max_bytes: usize,
) -> Result<Raster, String> {
    bitmap::set_budget(max_bytes);
    let mut st = State::default();
    let mut failed = None;
    if let Some(g) = globals {
        for seg in segment::parse(g)? {
            st.process(&seg)?;
        }
    }
    match segment::parse(data) {
        Ok(segs) => {
            for seg in &segs {
                match st.process(seg) {
                    Ok(Flow::Go) => {}
                    Ok(Flow::Stop) => break,
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
        }
        Err(e) => failed = Some(e),
    }
    let page = match (st.page, failed) {
        (Some(p), _) => p,
        (None, Some(e)) => return Err(e),
        (None, None) => return Err("没有页信息段".into()),
    };
    let mut b = page.bitmap;
    // 原地换算，不再分配也不再计费：预算耗尽时已经合成好的页仍然交得出去
    // [协议] JBIG2 的 1 是黑；输出灰度里黑是 0
    b.data
        .iter_mut()
        .for_each(|v| *v = if *v == 1 { 0 } else { 255 });
    Ok(Raster {
        width: u32::try_from(b.w).map_err(|_| "页太宽")?,
        height: u32::try_from(b.h).map_err(|_| "页太高")?,
        channels: 1,
        data: b.data,
    })
}
