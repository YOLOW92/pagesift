//! 版面检测：PP-DocLayoutV3（HGNetV2-L 骨干 + RT-DETR 式混合编码器 + 可变形注意力解码器），
//! 输入整页图，输出带类别的区域框（正文、标题、公式、公式编号、表格、图……），按预测的阅读顺序排。
//! 结构照 transformers 的 `modeling_pp_doclayout_v3.py`、`modeling_hgnet_v2.py`，预处理与后处理照
//! `image_processing_pp_doclayout_v3.py`（均 Apache-2.0）。掩码转多边形不做。
//!
//! 流程：预处理（[`preprocess`]，拉伸到 800×800）→ 骨干（[`hgnet`]）→ 混合编码器（[`encoder`]，
//! 含掩码特征）→ 选查询（`query`，掩码外接框作初始参考框）→ 解码器（[`decoder`]，可变形注意力在
//! `deform`）→ 后处理（`postprocess`）。

pub mod config;
pub mod decoder;
pub(crate) mod deform;
pub mod encoder;
pub mod hgnet;
pub(crate) mod layers;
pub(crate) mod postprocess;
pub mod preprocess;
pub(crate) mod query;

use std::path::Path;

use crate::Image;

/// 一个版面区域，坐标是原图像素。
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// 类别名（config.json 的 `id2label`，例如 `formula`、`formula_number`、`text`）。
    pub label: String,
    /// 细分类名（inference.yml 的 `label_list`，例如 `display_formula`、`inline_formula`）；
    /// 没有那个文件时同 `label`。
    pub kind: String,
    /// 分数，0–1。
    pub score: f32,
}

/// [协议] `post_process_object_detection` 的 threshold 缺省值，也是 inference.yml 的 draw_threshold
const SCORE_THRESHOLD: f32 = 0.5;

/// 载入好的版面模型。
#[derive(Debug, Clone)]
pub struct Layout {
    pre: preprocess::Preprocess,
    backbone: hgnet::Backbone,
    encoder: encoder::Encoder,
    decoder: decoder::Decoder,
    labels: Vec<String>,
    kinds: Vec<String>,
    /// 分数阈值；缺省同参考实现，可按需调整。
    pub threshold: f32,
}

impl Layout {
    /// `dir` 是模型目录：model.safetensors、config.json、preprocessor_config.json，可选 inference.yml。
    pub fn load(dir: &Path) -> Result<Layout, String> {
        let cfg = config::Config::from_json(&crate::read_json(dir, "config.json")?)?;
        let pre =
            preprocess::Preprocess::from_json(&crate::read_json(dir, "preprocessor_config.json")?)?;
        let w = crate::read_weights(dir)?;
        let kinds = std::fs::read_to_string(dir.join("inference.yml"))
            .map(|s| config::parse_label_list(&s))
            .unwrap_or_default();
        Ok(Layout {
            pre,
            // [协议] 权重名前缀
            backbone: hgnet::Backbone::load(&w, "model.backbone.model")?,
            encoder: encoder::Encoder::load(&w, &cfg)?,
            decoder: decoder::Decoder::load(&w, &cfg)?,
            kinds: if kinds.len() == cfg.labels.len() {
                kinds
            } else {
                cfg.labels.clone()
            },
            labels: cfg.labels,
            threshold: SCORE_THRESHOLD,
        })
    }

    /// 检测整页图的版面区域，按预测的阅读顺序返回。
    pub fn detect(&self, img: &Image) -> Result<Vec<Region>, String> {
        let raw = self.raw(img)?;
        let (q, classes) = raw.logits.tc();
        if classes != self.labels.len() {
            return Err(format!(
                "the model outputs {classes} classes but the config has {}",
                self.labels.len()
            ));
        }
        let ranks = postprocess::order_ranks(&raw.order.data, q);
        let dets = postprocess::select(
            &raw.logits.data,
            classes,
            &raw.boxes,
            &ranks,
            self.threshold,
            img.width as f32,
            img.height as f32,
        );
        Ok(dets
            .into_iter()
            .map(|d| Region {
                x0: d.xyxy[0],
                y0: d.xyxy[1],
                x1: d.xyxy[2],
                y1: d.xyxy[3],
                label: self.labels[d.class].clone(),
                kind: self.kinds[d.class].clone(),
                score: d.score,
            })
            .collect())
    }

    /// 网络的原始输出（后处理之前），用来和参考实现逐数对照。
    pub fn raw(&self, img: &Image) -> Result<decoder::Raw, String> {
        if img.width == 0 || img.height == 0 {
            return Err("empty image".into());
        }
        // [协议] `Image` 只有灰度（1）、RGB（3）两种
        if img.channels != 1 && img.channels != 3 {
            return Err(format!(
                "only grayscale or RGB images are supported, got {} channels",
                img.channels
            ));
        }
        if img.data.len() != img.width * img.height * img.channels {
            return Err("image data length does not match its width and height".into());
        }
        let x = self.pre.run(img);
        let feats = self.backbone.forward(&x);
        let enc = self.encoder.forward(&feats);
        drop(feats);
        Ok(self.decoder.forward(&enc.feats, &enc.mask_feat))
    }
}
