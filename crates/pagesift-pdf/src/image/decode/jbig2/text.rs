//! 文本区域解码过程（T.88 §6.4）和文本区域段（§7.4.3）。
//! 【待实现】接口已定。

use std::rc::Rc;

use super::RegionInfo;
use super::bitmap::Bitmap;
use super::huffman::Table;

/// 文本区域段（类型 4、6、7）区域信息之后的数据。`symbols` 是引用的各符号字典导出的符号
/// （SBSYMS，按引用顺序拼接），`tables` 是引用的码表段。
pub(super) fn region(
    _data: &[u8],
    _info: &RegionInfo,
    _symbols: &[Rc<Bitmap>],
    _tables: &[Rc<Table>],
) -> Result<Bitmap, String> {
    Err("文本区域尚未实现".into())
}
