//! 图样字典（T.88 §6.7、§7.4.4）和半色调区域（§6.6、§7.4.5）。
//! 【待实现】接口已定。

use super::RegionInfo;
use super::bitmap::Bitmap;

/// 图样字典段（类型 16）的数据：解出各图样。
pub(super) fn pattern_dict(_data: &[u8]) -> Result<Vec<Bitmap>, String> {
    Err("图样字典尚未实现".into())
}

/// 半色调区域段（类型 20、22、23）区域信息之后的数据；`patterns` 是引用的图样字典。
pub(super) fn region(
    _data: &[u8],
    _info: &RegionInfo,
    _patterns: &[Bitmap],
) -> Result<Bitmap, String> {
    Err("半色调区域尚未实现".into())
}
