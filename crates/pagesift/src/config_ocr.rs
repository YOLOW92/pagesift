//! `[gpu]` 和 `[ocr]`：扫描页 OCR 的开关、模型和设备。OCR 默认关；打开后第一张扫描图来时才下载模型。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use pagesift_ocr::models::{Model, Quality};
use pagesift_ocr::pdf::{Fetch, PdfOcr};
use serde::Deserialize;

use super::Config;

/// `[gpu]`。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gpu {
    /// `auto`（默认）、`off`、`cuda`、`cuda:N`。环境变量 `PAGESIFT_GPU` 优先。
    #[serde(default)]
    pub mode: GpuMode,
}

/// 用哪块显卡。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum GpuMode {
    /// 有能用的 N 卡就用，没有就用 CPU。
    #[default]
    Auto,
    Off,
    /// 指定用 N 卡（序号）；用不了时退回 CPU 并报 `gpu_fallback`。
    Cuda(u32),
}

impl std::str::FromStr for GpuMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            "cuda" => Ok(Self::Cuda(0)),
            _ => s
                .strip_prefix("cuda:")
                .and_then(|n| n.parse().ok())
                .map(Self::Cuda)
                .ok_or_else(|| format!("gpu mode must be auto, off, cuda or cuda:N, not {s:?}")),
        }
    }
}

impl TryFrom<String> for GpuMode {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

/// `[ocr]`。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ocr {
    /// 打开才会下载模型（默认关）。
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub quality: OcrQuality,
    /// 模型目录；缺省 `$XDG_CACHE_HOME/pagesift/models`（`~/.cache/pagesift/models`）。
    pub models_dir: Option<PathBuf>,
}

/// `auto`：有 GPU 且显存放得下时用中模型，否则小模型；`fast`：小模型；`accurate`：中模型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OcrQuality {
    #[default]
    Auto,
    Fast,
    Accurate,
}

impl From<OcrQuality> for Quality {
    fn from(q: OcrQuality) -> Self {
        match q {
            OcrQuality::Auto => Quality::Auto,
            OcrQuality::Fast => Quality::Fast,
            OcrQuality::Accurate => Quality::Accurate,
        }
    }
}

impl Config {
    /// 实际生效的 GPU 模式：`PAGESIFT_GPU` 优先于 `[gpu] mode`。
    pub fn gpu_mode(&self) -> Result<GpuMode> {
        match std::env::var("PAGESIFT_GPU") {
            Ok(v) if !v.is_empty() => v
                .parse()
                .map_err(|e: String| anyhow!(e).context("PAGESIFT_GPU")),
            _ => Ok(self.gpu.mode),
        }
    }

    /// 模型目录。
    pub fn models_root(&self) -> Result<PathBuf> {
        self.ocr
            .models_dir
            .clone()
            .or_else(pagesift_ocr::models::cache_root)
            .ok_or_else(|| {
                anyhow!(
                    "no model directory: set [ocr] models_dir or --models-dir (HOME is not set)"
                )
            })
    }

    /// 这个配置要用的模型（检测、识别）。
    pub fn ocr_models(&self) -> (Model, Model) {
        Quality::from(self.ocr.quality).models(false)
    }

    /// 组装 OCR 引擎（懒加载，这里不碰网络和模型文件）；没打开时返回 `None`。
    pub(super) fn ocr_engine(
        &self,
        fetcher: &pagesift_net::FetcherConfig,
    ) -> Result<Option<Arc<dyn pagesift_pdf::PageOcr>>> {
        if !self.ocr.enabled {
            return Ok(None);
        }
        let root = self.models_root()?;
        let (det, rec) = self.ocr_models();
        let fetch: Fetch = {
            let (root, fetcher) = (root.clone(), fetcher.clone());
            Arc::new(move |m: &Model| crate::models::fetch_blocking(&fetcher, &root, m))
        };
        Ok(Some(Arc::new(PdfOcr::new(root, det, rec, Some(fetch)))))
    }
}
