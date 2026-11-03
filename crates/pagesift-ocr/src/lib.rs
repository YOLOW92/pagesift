//! 扫描页 OCR：PP-OCRv6 的文字检测（DB）和文字识别（SVTR + CTC），纯 Rust，CPU 或 N 卡上跑
//! （设备见 [`device`]，GPU 不可用时自动退回 CPU 并报告原因）。
//! 权重是 PaddlePaddle 在 Hugging Face 上发布的 safetensors（Apache-2.0），首次使用时下载
//! （见 [`models`]），不随本仓库分发。
//!
//! 流程：整页图 → [`det::Det::detect`] 得到文字行的四边形 → 逐行裁出、拉正 →
//! [`rec::Rec::recognize`] 得到文字和置信度。

pub mod det;
pub mod device;
pub mod formula;
pub mod formula_enc;
pub mod layout;
pub mod lcnet;
pub mod math;
pub mod models;
pub mod pdf;
pub mod pipeline;
pub mod rec;
mod script;

pub use device::{Device, DeviceStatus, ModelChoice, Quality, select_models};

/// 一张 8 位图：灰度（1 通道）或 RGB（3 通道），按行存放、无行间填充。
#[derive(Debug, Clone)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u8>,
}

impl Image {
    /// 取 (x, y) 处第 c 个通道；灰度图三个通道都返回同一个值。
    #[inline]
    pub fn at(&self, x: usize, y: usize, c: usize) -> u8 {
        let c = if self.channels == 1 { 0 } else { c };
        self.data[(y * self.width + x) * self.channels + c]
    }
}

/// 图上的四边形：左上、右上、右下、左下四个角（像素坐标）。
pub type Quad = [(f32, f32); 4];

/// 识别出的一行字。
#[derive(Debug, Clone)]
pub struct Line {
    pub quad: Quad,
    pub text: String,
    /// 识别置信度（CTC 各字的概率平均），0–1。
    pub score: f32,
    /// 行间公式：`text` 是 `$$…$$`。
    pub formula: bool,
}

/// 读模型目录下的 config.json。
pub(crate) fn read_json(dir: &std::path::Path, name: &str) -> Result<serde_json::Value, String> {
    let p = dir.join(name);
    let s = std::fs::read(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
    serde_json::from_slice(&s).map_err(|e| format!("{} is not valid JSON: {e}", p.display()))
}

/// 读模型目录下的 model.safetensors。
pub(crate) fn read_weights(dir: &std::path::Path) -> Result<pagesift_infer::Weights, String> {
    let p = dir.join("model.safetensors");
    let b = std::fs::read(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
    pagesift_infer::Weights::from_bytes(&b)
}
