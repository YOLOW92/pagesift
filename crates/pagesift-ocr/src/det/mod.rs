//! 文字检测（PP-OCRv6 small det：LCNetV4 骨干 + RepLKFPN 颈部 + DB 头 + DB 后处理）。
//!
//! 流程：预处理（[`preprocess`]）→ 骨干（[`crate::lcnet`]）→ 颈部与头（[`neck`]）得到与
//! 网络输入同尺寸的文字概率图 → DB 后处理（[`postprocess`]，几何在 [`geometry`]、轮廓在
//! [`contour`]）→ 映射回原图坐标、排成阅读顺序。

pub mod contour;
pub mod geometry;
pub mod neck;
pub mod postprocess;
pub mod preprocess;

use std::path::Path;

use crate::{Image, Quad, lcnet};
use postprocess::{DbParams, ProbMap};
use preprocess::Preprocess;

/// [策略] 阅读顺序里“同一行”的容差（原图像素），同 PaddleOCR `sorted_boxes` 的 10
const LINE_TOL: f32 = 10.0;

/// 载入好的检测模型。
pub struct Det {
    pre: Preprocess,
    backbone: lcnet::Backbone,
    neck: neck::Neck,
    head: neck::Head,
    /// DB 后处理参数；缺省同参考实现，可按需调整。
    pub params: DbParams,
}

impl Det {
    /// `dir` 是模型目录：model.safetensors、config.json、preprocessor_config.json。
    pub fn load(dir: &Path) -> Result<Det, String> {
        let cfg = crate::read_json(dir, "config.json")?;
        let pre = Preprocess::from_json(&crate::read_json(dir, "preprocessor_config.json")?)?;
        let w = crate::read_weights(dir)?;
        let bcfg = lcnet::Config::from_json(&cfg)?;
        let ncfg = neck::NeckConfig::from_json(&cfg)?;
        Ok(Det {
            pre,
            // [协议] 权重名前缀：model.backbone.encoder、model.neck、head
            backbone: lcnet::Backbone::load(&w, "model.backbone.encoder", &bcfg)?,
            neck: neck::Neck::load(&w, "model.neck", &ncfg)?,
            head: neck::Head::load(&w, "head", &ncfg)?,
            params: DbParams::default(),
        })
    }

    /// 整页图 → 文字概率图（网络输入尺寸，按行存放）及其宽、高。
    pub fn prob_map(&self, page: &Image) -> Result<(Vec<f32>, usize, usize), String> {
        if page.width == 0 || page.height == 0 {
            return Err("图像为空".into());
        }
        if page.channels != 1 && page.channels != 3 {
            return Err(format!("只支持灰度或 RGB 图，实际 {} 通道", page.channels));
        }
        let x = self.pre.run(page);
        let feats = self.backbone.forward(&x);
        let fused = self.neck.forward(&feats)?;
        let prob = self.head.forward(&fused);
        let (_, h, w) = prob.chw();
        if (h, w) != (x.shape[1], x.shape[2]) {
            return Err(format!(
                "概率图尺寸 {h}×{w} 与输入 {}×{} 不符",
                x.shape[1], x.shape[2]
            ));
        }
        Ok((prob.data, w, h))
    }

    /// 检测整页图里的文字行，返回各行的四边形（原图像素坐标），从上到下、从左到右排好。
    pub fn detect(&self, page: &Image) -> Result<Vec<Quad>, String> {
        Ok(self
            .detect_scored(page)?
            .into_iter()
            .map(|(q, _)| q)
            .collect())
    }

    /// 同 [`Det::detect`]，并给出每个框的平均概率。
    pub fn detect_scored(&self, page: &Image) -> Result<Vec<(Quad, f32)>, String> {
        let (prob, w, h) = self.prob_map(page)?;
        Ok(self.boxes_from_prob(&prob, w, h, page.width, page.height))
    }

    /// 只做后处理：`w × h` 的概率图 → 原图（`page_w × page_h`）坐标下排好序的框和分数。
    pub fn boxes_from_prob(
        &self,
        prob: &[f32],
        w: usize,
        h: usize,
        page_w: usize,
        page_h: usize,
    ) -> Vec<(Quad, f32)> {
        let pm = ProbMap {
            data: prob,
            width: w,
            height: h,
        };
        let mut boxes = postprocess::boxes_from_prob(&pm, page_w, page_h, &self.params);
        postprocess::sort_reading_order(&mut boxes, LINE_TOL);
        boxes
    }
}
