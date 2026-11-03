//! 文字检测（PP-OCRv6 small/medium det：LCNetV4 + RepLKFPN + DB 头 + DB 后处理）。
//! 【待实现】接口已定。

use std::path::Path;

use crate::{Image, Quad};

/// 载入好的检测模型。
pub struct Det {}

impl Det {
    /// `dir` 是模型目录：model.safetensors、config.json、preprocessor_config.json。
    pub fn load(_dir: &Path) -> Result<Det, String> {
        Err("检测模型尚未实现".into())
    }

    /// 检测整页图里的文字行，返回各行的四边形（原图像素坐标），从上到下、从左到右排好。
    pub fn detect(&self, _page: &Image) -> Result<Vec<Quad>, String> {
        Err("检测模型尚未实现".into())
    }
}
