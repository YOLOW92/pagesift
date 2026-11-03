//! 符号字典解码过程（T.88 §6.5）和符号字典段（§7.4.2）。
//! 【待实现】接口已定。

use std::rc::Rc;

use super::bitmap::Bitmap;
use super::huffman::Table;

/// 留给后续符号字典沿用的算术解码上下文（§7.4.2.1.1 的“位图编码上下文保留”标志）。
#[derive(Debug)]
pub(super) struct Retained {}

/// 符号字典段的数据。`input` 是引用的各符号字典导出的符号（SDINSYMS，按引用顺序），
/// `tables` 是引用的码表段（按引用顺序，自定义表从这里取），`prior` 是引用的最后一个符号字典
/// 保留下来的上下文。返回导出的符号（SDEXSYMS）和要保留的上下文（标志要求保留时）。
pub(super) fn dictionary(
    _data: &[u8],
    _input: &[Rc<Bitmap>],
    _tables: &[Rc<Table>],
    _prior: Option<&Retained>,
) -> Result<(Vec<Rc<Bitmap>>, Option<Retained>), String> {
    Err("符号字典尚未实现".into())
}
