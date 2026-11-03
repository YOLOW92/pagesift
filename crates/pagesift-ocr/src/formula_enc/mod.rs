//! PP-FormulaNet_plus-L 公式识别的图像编码器（到解码器交叉注意力的 memory 为止）。结构照
//! transformers 的 `modeling_pp_formulanet.py` 与 `image_processing_pp_formulanet.py`
//! （Apache-2.0，PaddlePaddle 与 HuggingFace）里的 `PPFormulaNetVisionModel`：
//!
//! ```text
//! 图 ──preprocess──▶ [3, 768, 768]
//!    ──patch_embed（16×16 卷积，步长 16）──▶ [48·48, 768] + pos_embed
//!    ──12 层 ViT（窗口 14，第 2、5、8、11 层全局；分解相对位置）──▶ [2304, 768]
//!    ──neck（1×1 卷积、LN、3×3 卷积、LN）──▶ [256, 48, 48]
//!    ──multi_modal_projector（两次 3×3 步长 2 卷积、两层 Linear）──▶ [144, 512]
//! ```
//!
//! [`Encoder::encode`] 的输出就是参考实现的 `pooler_output`：形状 `[144, 512]`，
//! 第 i 行对应 12×12 网格上 (i / 12, i % 12) 处（行优先）、原图约 64×64 像素的一块，
//! 512 维即解码器的 `d_model`。`PPFormulaNetModel.forward` 把它原样作为 `encoder_hidden_states`
//! 交给解码器，各层交叉注意力的 k/v 投影直接作用在它上面，中间没有别的变换。

mod attention;
mod neck;
mod par;
mod preprocess;
mod vit;

use std::path::Path;

use pagesift_infer::{Conv2d, Tensor, Weights, ops};

pub use preprocess::{SIZE, preprocess};

/// [协议] 权重名前缀：`PPFormulaNetForConditionalGeneration.model.encoder`
const PREFIX: &str = "model.encoder";

/// config.json 里 `vision_config` 用到的部分。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub image_size: usize,
    pub patch_size: usize,
    pub hidden_size: usize,
    pub layers: usize,
    pub heads: usize,
    pub window_size: usize,
    pub global_attn_indexes: Vec<usize>,
    pub layer_norm_eps: f32,
}

impl Config {
    /// 从 config.json 的 `vision_config` 读。只支持 `use_abs_pos`、`use_rel_pos` 都为真、
    /// `hidden_act = "gelu"`（PP-FormulaNet_plus-L 就是这样）。
    pub fn from_json(v: &serde_json::Value) -> Result<Config, String> {
        let c = &v["vision_config"];
        let int = |k: &str| {
            c[k].as_u64()
                .map(|x| x as usize)
                .ok_or_else(|| format!("vision_config has no integer {k}"))
        };
        for k in ["use_abs_pos", "use_rel_pos"] {
            if c[k].as_bool() != Some(true) {
                return Err(format!("only {k} = true is supported"));
            }
        }
        if c["hidden_act"].as_str().unwrap_or("gelu") != "gelu" {
            return Err("only hidden_act = gelu is supported".into());
        }
        let global_attn_indexes = c["global_attn_indexes"]
            .as_array()
            .ok_or("vision_config has no global_attn_indexes")?
            .iter()
            .map(|x| {
                x.as_u64()
                    .map(|x| x as usize)
                    .ok_or("global_attn_indexes must be integers")
            })
            .collect::<Result<_, _>>()?;
        let cfg = Config {
            image_size: int("image_size")?,
            patch_size: int("patch_size")?,
            hidden_size: int("hidden_size")?,
            layers: int("num_hidden_layers")?,
            heads: int("num_attention_heads")?,
            window_size: int("window_size")?,
            global_attn_indexes,
            layer_norm_eps: c["layer_norm_eps"]
                .as_f64()
                .ok_or("vision_config has no layer_norm_eps")? as f32,
        };
        if cfg.image_size != SIZE {
            return Err(format!(
                "preprocessing always outputs {SIZE}, the config says {}",
                cfg.image_size
            ));
        }
        if cfg.patch_size == 0 || !cfg.image_size.is_multiple_of(cfg.patch_size) {
            return Err("image_size is not a multiple of patch_size".into());
        }
        if cfg.heads == 0 || !cfg.hidden_size.is_multiple_of(cfg.heads) {
            return Err("hidden_size is not divisible by the number of heads".into());
        }
        Ok(cfg)
    }

    /// 特征图边长（768 / 16 = 48）。
    pub fn grid(&self) -> usize {
        self.image_size / self.patch_size
    }
}

/// 载入好的编码器。
pub struct Encoder {
    cfg: Config,
    patch: Conv2d,
    /// `[grid², hidden]`，与 patch 序列逐元素相加
    pos: Vec<f32>,
    layers: Vec<vit::Layer>,
    neck: neck::Neck,
    proj: neck::Projector,
}

impl Encoder {
    /// `dir` 是模型目录：model.safetensors、config.json。
    pub fn load(dir: &Path) -> Result<Encoder, String> {
        let cfg = Config::from_json(&crate::read_json(dir, "config.json")?)?;
        Encoder::from_weights(&crate::read_weights(dir)?, cfg)
    }

    /// 从已读入的权重建（解码器可共用同一份 [`Weights`]）。用到的参数都复制出来，之后可丢掉 `w`。
    pub fn from_weights(w: &Weights, cfg: Config) -> Result<Encoder, String> {
        let (g, c) = (cfg.grid(), cfg.hidden_size);
        let ps = cfg.patch_size;
        let patch = Conv2d::load(
            w,
            &format!("{PREFIX}.patch_embed.projection"),
            (ps, ps),
            (0, 0),
            (1, 1),
            1,
        )?;
        let pos = w.get(&format!("{PREFIX}.pos_embed"))?;
        // [协议] pos_embed 是 [1, grid, grid, hidden]（通道在最后，与序列布局一致）
        if pos.shape != [1, g, g, c] {
            return Err(format!(
                "pos_embed has shape {:?}, expected [1, {g}, {g}, {c}]",
                pos.shape
            ));
        }
        let mut layers = Vec::with_capacity(cfg.layers);
        for i in 0..cfg.layers {
            let window = if cfg.global_attn_indexes.contains(&i) {
                0
            } else {
                cfg.window_size
            };
            layers.push(vit::Layer::load(
                w,
                &format!("{PREFIX}.layers.{i}"),
                cfg.heads,
                window,
                g,
                cfg.layer_norm_eps,
            )?);
        }
        Ok(Encoder {
            patch,
            pos: pos.data.clone(),
            layers,
            neck: neck::Neck::load(w, &format!("{PREFIX}.neck"))?,
            proj: neck::Projector::load(w, &format!("{PREFIX}.multi_modal_projector"))?,
            cfg,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// `x` 是 [`preprocess`] 的输出 `[3, 768, 768]`，返回解码器交叉注意力的 memory `[144, 512]`。
    pub fn encode(&self, x: &Tensor) -> Result<Tensor, String> {
        self.encode_traced(x, |_, _| {})
    }

    /// 同 [`encode`](Self::encode)，每一段的输出都交给 `trace(名字, 张量)` 看一眼（检查用）。
    pub fn encode_traced(
        &self,
        x: &Tensor,
        mut trace: impl FnMut(&str, &Tensor),
    ) -> Result<Tensor, String> {
        let s = self.cfg.image_size;
        // [协议] 输入三通道（`num_channels`）
        if x.shape != [3, s, s] {
            return Err(format!("input must be [3, {s}, {s}], got {:?}", x.shape));
        }
        let g = self.cfg.grid();
        let mut h = ops::chw_to_seq(&self.patch.forward(x));
        for (v, p) in h.data.iter_mut().zip(&self.pos) {
            *v += p;
        }
        trace("patch_embed+pos", &h);
        for (i, l) in self.layers.iter().enumerate() {
            h = l.forward(h, g);
            trace(&format!("layer{i}"), &h);
        }
        let n = self.neck.forward(&h, g);
        trace("neck", &n);
        let m = self.proj.forward(&n);
        trace("projector", &m);
        Ok(m)
    }
}
