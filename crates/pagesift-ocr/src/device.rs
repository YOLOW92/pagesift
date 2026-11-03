//! 设备与模型的选择：跑在哪（[`Device`]）、实际跑在哪（[`DeviceStatus`]）、用哪档模型（[`select_models`]）。

use pagesift_gpu::GpuInfo;

use crate::models::{MEDIUM_DET, MEDIUM_REC, Model, SMALL_DET, SMALL_REC};

/// 想用的设备。配置 `[gpu] mode` 的对应：`off` → `Cpu`，`cuda` → `Gpu { index: 0 }`，
/// `cuda:N` → `Gpu { index: N }`；`auto` 也是 `Gpu { index: 0 }`，打不开时退回 CPU。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Gpu { index: usize },
}

/// 实际用的设备，以及想用 GPU 却退回 CPU 的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStatus {
    /// 调用方要的设备。
    pub requested: Device,
    /// 实际用的设备。
    pub actual: Device,
    /// `actual` 是 GPU 时它的情况（显存空闲量是打开时的值）。
    pub gpu: Option<GpuInfo>,
    /// 退回 CPU 的原因；没有退回（包括本来就要 CPU）时为 None。`actual` 仍是 GPU 而这里有值，
    /// 说明只有个别页、行因为显存不够在 CPU 上算过。
    pub fallback: Option<String>,
}

impl DeviceStatus {
    /// 是否想用 GPU 却退回了 CPU（对应警告 `gpu_fallback`）。
    pub fn fell_back(&self) -> bool {
        self.fallback.is_some()
    }

    /// 不想用 GPU。
    pub(crate) fn cpu() -> DeviceStatus {
        DeviceStatus {
            requested: Device::Cpu,
            actual: Device::Cpu,
            gpu: None,
            fallback: None,
        }
    }

    /// GPU 打不开，退回 CPU。
    pub(crate) fn fallback(index: usize, why: String) -> DeviceStatus {
        DeviceStatus {
            requested: Device::Gpu { index },
            actual: Device::Cpu,
            gpu: None,
            fallback: Some(why),
        }
    }

    /// GPU 打开了。
    pub(crate) fn on_gpu(index: usize, info: GpuInfo) -> DeviceStatus {
        let device = Device::Gpu { index };
        DeviceStatus {
            requested: device,
            actual: device,
            gpu: Some(info),
            fallback: None,
        }
    }
}

/// 配置 `[ocr] quality`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// 有 GPU 且显存放得下 medium 就用 medium，否则 small。
    Auto,
    /// small。
    Fast,
    /// medium，不看设备（CPU 上也用，只是慢）。
    Accurate,
}

/// 选中的模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelChoice {
    pub det: Model,
    pub rec: Model,
    /// “small” 或 “medium”。
    pub tier: &'static str,
}

impl ModelChoice {
    fn small() -> ModelChoice {
        ModelChoice {
            det: SMALL_DET,
            rec: SMALL_REC,
            tier: "small",
        }
    }

    fn medium() -> ModelChoice {
        ModelChoice {
            det: MEDIUM_DET,
            rec: MEDIUM_REC,
            tier: "medium",
        }
    }
}

/// 一个模型文件清单里 model.safetensors 的字节数。
fn weight_bytes(m: &Model) -> u64 {
    m.files
        .iter()
        .filter(|f| f.name == "model.safetensors")
        .map(|f| f.size)
        .sum()
}

/// medium 在显存里至少要占多少字节（权重加一页的工作内存）。
pub fn medium_vram_bytes() -> u64 {
    // [推导] 工作内存按权重的同量估：显存层的特征图、im2col 缓冲和权重同一量级，放不下时工人还会减小批次
    2 * (weight_bytes(&MEDIUM_DET) + weight_bytes(&MEDIUM_REC))
}

/// 按质量档和设备选模型。`Quality::Auto` 用 [`pagesift_gpu::probe`] 查空闲显存；查不到（没有 GPU、
/// `PAGESIFT_GPU=off`）时选 small。
pub fn select_models(quality: Quality, device: Device) -> ModelChoice {
    match (quality, device) {
        (Quality::Fast, _) | (Quality::Auto, Device::Cpu) => ModelChoice::small(),
        (Quality::Accurate, _) => ModelChoice::medium(),
        (Quality::Auto, Device::Gpu { index }) => match pagesift_gpu::probe(index) {
            Ok(info) if info.memory_free >= medium_vram_bytes() => ModelChoice::medium(),
            _ => ModelChoice::small(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 按质量档选模型() {
        let g = Device::Gpu { index: 0 };
        assert_eq!(select_models(Quality::Fast, g).tier, "small");
        assert_eq!(select_models(Quality::Auto, Device::Cpu).tier, "small");
        assert_eq!(select_models(Quality::Accurate, Device::Cpu).tier, "medium");
    }

    #[test]
    fn 不存在的显卡退回_cpu_并带原因() {
        let e = pagesift_gpu::probe(9999).unwrap_err();
        let s = DeviceStatus::fallback(9999, e);
        assert_eq!(s.actual, Device::Cpu);
        assert!(s.fell_back());
        assert_eq!(s.requested, Device::Gpu { index: 9999 });
        assert_eq!(
            select_models(Quality::Auto, Device::Gpu { index: 9999 }).tier,
            "small"
        );
    }
}
