//! 文字识别（PP-OCRv6 small/medium rec）。
//! 【待实现】接口已定。

use std::path::Path;

use crate::Image;

/// 载入好的识别模型。
pub struct Rec {}

impl Rec {
    /// `dir` 是模型目录：model.safetensors、config.json、preprocessor_config.json（含字符表）。
    pub fn load(_dir: &Path) -> Result<Rec, String> {
        Err("识别模型尚未实现".into())
    }

    /// 识别一行字的图（已裁出、拉正成水平的一行）。返回文字和置信度。
    pub fn recognize(&self, _line: &Image) -> Result<(String, f32), String> {
        Err("识别模型尚未实现".into())
    }
}
