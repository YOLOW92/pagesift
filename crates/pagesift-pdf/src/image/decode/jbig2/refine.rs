//! 通用细化区域解码过程（T.88 §6.3）和细化区域段（§7.4.7）。
//! 【待实现】接口已定，供符号字典（REFAGG）、文本区域（SBREFINE）和细化区域段调用。

use super::RegionInfo;
use super::bitmap::Bitmap;
use super::mq::{Cx, Mq};

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

/// 通用细化区域解码（§6.3.5.6）。`cx` 至少 [`context_count`] 个，由调用方持有（文本区域、
/// 符号字典里多次细化共用）。
pub(super) fn decode(_mq: &mut Mq, _cx: &mut [Cx], _p: &Params) -> Result<Bitmap, String> {
    Err("通用细化区域尚未实现".into())
}

/// 细化区域段（类型 40、42、43）区域信息之后的数据（§7.4.7）。`reference` 已按区域裁好
/// （引用中间区域时就是它的位图），GRREFERENCEDX/DY 为 0。
pub(super) fn segment(
    _data: &[u8],
    _info: &RegionInfo,
    _reference: &Bitmap,
) -> Result<Bitmap, String> {
    Err("细化区域段尚未实现".into())
}
