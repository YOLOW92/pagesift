//! mBART 式文本解码器与贪心生成，对应参考实现的 `PPFormulaNetTextModel`、
//! `PPFormulaNetDecoderLayer` 和 `PPFormulaNetForConditionalGeneration`：
//!
//! 词向量 × √d_model + 位置向量（下标偏移 2）→ layernorm_embedding →
//! 每层（前置 LayerNorm）：因果自注意力、对 memory 的交叉注意力、前馈，都带残差 →
//! 末尾 layer_norm → lm_head（无偏置）。
//!
//! 自注意力的 K、V 逐步追加，交叉注意力的 K、V 每层只算一次（见 [`Cache`]）。

use std::path::Path;

use pagesift_infer::{Activation, LayerNorm, Linear, Tensor, Weights, ops};
use serde_json::Value;

use super::attention::Attention;

/// [协议] `nn.LayerNorm` 的缺省 eps（参考实现没有改它）
const LN_EPS: f32 = 1e-5;
/// [协议] `PPFormulaNetLearnedPositionalEmbedding.offset`：位置下标统一加 2
const POSITION_OFFSET: usize = 2;
/// [协议] 权重名前缀：`PPFormulaNetForConditionalGeneration.model.decoder`
const PREFIX: &str = "model.decoder";

struct Layer {
    self_ln: LayerNorm,
    self_attn: Attention,
    cross_ln: LayerNorm,
    cross_attn: Attention,
    final_ln: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    act: Activation,
}

/// 一层的缓存：自注意力的 K、V（`[已生成, D]`，逐步追加），交叉注意力的 K、V（`[L, D]`，只算一次）。
struct LayerCache {
    k: Vec<f32>,
    v: Vec<f32>,
    cross_k: Vec<f32>,
    cross_v: Vec<f32>,
}

/// 一次生成的缓存。
pub(super) struct Cache {
    layers: Vec<LayerCache>,
    /// 已经喂进去的 token 数，也是下一个 token 的位置。
    len: usize,
    /// memory 的长度 L。
    memory_len: usize,
}

impl Layer {
    fn load(w: &Weights, p: &str, heads: usize, act: Activation) -> Result<Layer, String> {
        Ok(Layer {
            self_ln: LayerNorm::load(w, &format!("{p}.self_attn_layer_norm"), LN_EPS)?,
            self_attn: Attention::load(w, &format!("{p}.self_attn"), heads)?,
            cross_ln: LayerNorm::load(w, &format!("{p}.encoder_attn_layer_norm"), LN_EPS)?,
            cross_attn: Attention::load(w, &format!("{p}.encoder_attn"), heads)?,
            final_ln: LayerNorm::load(w, &format!("{p}.final_layer_norm"), LN_EPS)?,
            fc1: Linear::load(w, &format!("{p}.fc1"))?,
            fc2: Linear::load(w, &format!("{p}.fc2"))?,
            act,
        })
    }

    /// `x` 是新喂入的 `[T, D]`，`past` 是这之前已缓存的长度。
    fn forward(&self, x: Tensor, c: &mut LayerCache, past: usize, memory_len: usize) -> Tensor {
        let n = self.self_ln.forward(&x);
        c.k.extend_from_slice(&self.self_attn.k.forward(&n).data);
        c.v.extend_from_slice(&self.self_attn.v.forward(&n).data);
        let a = self.self_attn.attend(&n, &c.k, &c.v, |i| past + i + 1);
        let x = ops::add(&x, &a);

        let n = self.cross_ln.forward(&x);
        let a = self
            .cross_attn
            .attend(&n, &c.cross_k, &c.cross_v, |_| memory_len);
        let x = ops::add(&x, &a);

        let mut h = self.fc1.forward(&self.final_ln.forward(&x));
        self.act.apply_all(&mut h);
        ops::add(&x, &self.fc2.forward(&h))
    }
}

/// 载入好的解码器。
pub struct Decoder {
    /// `[词表, D]`
    embed_tokens: Tensor,
    /// `[最大位置 + 2, D]`
    embed_positions: Tensor,
    embed_scale: f32,
    layernorm_embedding: LayerNorm,
    layers: Vec<Layer>,
    layer_norm: LayerNorm,
    lm_head: Linear,
    d_model: usize,
    start: u32,
    eos: u32,
    forced_eos: Option<u32>,
    max_length: usize,
}

fn cfg_usize(c: &Value, key: &str) -> Result<usize, String> {
    c[key]
        .as_u64()
        .map(|v| v as usize)
        .ok_or_else(|| format!("the config has no {key}"))
}

impl Decoder {
    /// `dir` 是模型目录：model.safetensors、config.json、generation_config.json。
    pub fn load(dir: &Path) -> Result<Decoder, String> {
        let cfg = crate::read_json(dir, "config.json")?;
        let generation = crate::read_json(dir, "generation_config.json")?;
        let w = crate::read_weights(dir)?;
        Decoder::from_weights(&w, &cfg["text_config"], &generation)
    }

    /// 从已读入的权重构造（和图像编码器共用一次读文件时用）。
    /// `text` 是 config.json 的 `text_config`，`generation` 是 generation_config.json。
    pub fn from_weights(w: &Weights, text: &Value, generation: &Value) -> Result<Decoder, String> {
        let d_model = cfg_usize(text, "d_model")?;
        let heads = cfg_usize(text, "decoder_attention_heads")?;
        if heads == 0 || !d_model.is_multiple_of(heads) {
            return Err(format!(
                "d_model {d_model} is not divisible by {heads} heads"
            ));
        }
        let act = match text["activation_function"].as_str().unwrap_or("") {
            "gelu" => Activation::Gelu,
            "relu" => Activation::Relu,
            "silu" | "swish" => Activation::Silu,
            a => return Err(format!("unsupported activation {a:?}")),
        };
        let layers = (0..cfg_usize(text, "decoder_layers")?)
            .map(|i| Layer::load(w, &format!("{PREFIX}.layers.{i}"), heads, act))
            .collect::<Result<Vec<_>, _>>()?;
        let embed_tokens = w.get(&format!("{PREFIX}.embed_tokens.weight"))?.clone();
        let embed_positions = w.get(&format!("{PREFIX}.embed_positions.weight"))?.clone();
        if embed_tokens.tc().1 != d_model || embed_positions.tc().1 != d_model {
            return Err(format!(
                "token or position embedding size does not match d_model {d_model}"
            ));
        }
        let embed_scale = if text["scale_embedding"].as_bool().unwrap_or(false) {
            (d_model as f32).sqrt()
        } else {
            1.0
        };
        let token = |key: &str| cfg_usize(generation, key).map(|v| v as u32);
        Ok(Decoder {
            embed_tokens,
            embed_positions,
            embed_scale,
            layernorm_embedding: LayerNorm::load(
                w,
                &format!("{PREFIX}.layernorm_embedding"),
                LN_EPS,
            )?,
            layers,
            layer_norm: LayerNorm::load(w, &format!("{PREFIX}.layer_norm"), LN_EPS)?,
            lm_head: Linear::load(w, "lm_head")?,
            d_model,
            start: token("decoder_start_token_id")?,
            eos: token("eos_token_id")?,
            forced_eos: token("forced_eos_token_id").ok(),
            max_length: cfg_usize(generation, "max_length")?,
        })
    }

    /// generation_config.json 的 `max_length`（含起始 token 的序列总长），传给 [`generate`](Self::generate)。
    pub fn max_length(&self) -> usize {
        self.max_length
    }

    /// 贪心生成。`memory` 是编码器输出 `[L, d_model]`（参考实现的 `encoder_hidden_states`）。
    ///
    /// `max_len` 的含义同 `generate(max_length=…)`：序列总长的上限，含起始 token；序列长到
    /// `max_len − 1` 时下一个 token 强制为 `forced_eos_token_id`
    /// （`ForcedEOSTokenLogitsProcessor`）。超过位置向量表的长度时取表长。
    ///
    /// 返回生成的 token，不含起始 token 和结尾的 eos。
    pub fn generate(&self, memory: &Tensor, max_len: usize) -> Result<Vec<u32>, String> {
        let max_len = max_len.min(self.max_positions());
        let mut cache = self.new_cache(memory)?;
        let mut last = self.start;
        let mut out = Vec::new();
        // 序列长度 = 起始 token + 已生成的
        for len in 1..max_len {
            let hidden = self.forward(&[last], &mut cache)?;
            let next = match self.forced_eos {
                Some(f) if len + 1 == max_len => f,
                _ => argmax(&self.logits(&hidden)?),
            };
            if next == self.eos {
                break;
            }
            out.push(next);
            last = next;
        }
        Ok(out)
    }

    /// 位置向量表能容纳的序列长度。
    fn max_positions(&self) -> usize {
        self.embed_positions.tc().0.saturating_sub(POSITION_OFFSET)
    }

    /// 建一次生成的缓存：交叉注意力的 K、V 每层在这里算一次。
    pub(super) fn new_cache(&self, memory: &Tensor) -> Result<Cache, String> {
        if memory.shape.len() != 2 || memory.shape[1] != self.d_model || memory.shape[0] == 0 {
            return Err(format!(
                "memory must be [L, {}], got {:?}",
                self.d_model, memory.shape
            ));
        }
        let layers = self
            .layers
            .iter()
            .map(|l| LayerCache {
                k: Vec::new(),
                v: Vec::new(),
                cross_k: l.cross_attn.k.forward(memory).data,
                cross_v: l.cross_attn.v.forward(memory).data,
            })
            .collect();
        Ok(Cache {
            layers,
            len: 0,
            memory_len: memory.shape[0],
        })
    }

    /// 喂入新的 token（接在缓存之后），返回这些 token 的隐状态 `[T, D]`。
    pub(super) fn forward(&self, ids: &[u32], cache: &mut Cache) -> Result<Tensor, String> {
        let (vocab, d) = self.embed_tokens.tc();
        let (past, memory_len) = (cache.len, cache.memory_len);
        if past + ids.len() > self.max_positions() {
            return Err(format!(
                "sequence length {} exceeds the position table of {}",
                past + ids.len(),
                self.max_positions()
            ));
        }
        let mut x = Vec::with_capacity(ids.len() * d);
        for (i, &id) in ids.iter().enumerate() {
            let id = id as usize;
            if id >= vocab {
                return Err(format!("token {id} is outside the vocabulary of {vocab}"));
            }
            let tok = &self.embed_tokens.data[id * d..(id + 1) * d];
            let at = (past + i + POSITION_OFFSET) * d;
            let pos = &self.embed_positions.data[at..at + d];
            x.extend(tok.iter().zip(pos).map(|(t, p)| t * self.embed_scale + p));
        }
        let mut x = self
            .layernorm_embedding
            .forward(&Tensor::new(vec![ids.len(), d], x));
        for (layer, c) in self.layers.iter().zip(&mut cache.layers) {
            x = layer.forward(x, c, past, memory_len);
        }
        cache.len += ids.len();
        Ok(self.layer_norm.forward(&x))
    }

    /// 隐状态最后一行的 logits。出现 NaN 或无穷时报错，不让它变成一串看似正常的 token。
    pub(super) fn logits(&self, hidden: &Tensor) -> Result<Vec<f32>, String> {
        let (t, d) = hidden.tc();
        let last = Tensor::new(vec![1, d], hidden.data[(t - 1) * d..].to_vec());
        let y = self.lm_head.forward(&last).data;
        if y.iter().any(|v| !v.is_finite()) {
            return Err("NaN or infinity in the logits".into());
        }
        Ok(y)
    }
}

/// 最大值的下标，相同时取最前面的（`torch.argmax`）。
fn argmax(v: &[f32]) -> u32 {
    let (mut arg, mut m) = (0, f32::NEG_INFINITY);
    for (i, &x) in v.iter().enumerate() {
        if x > m {
            (arg, m) = (i, x);
        }
    }
    arg as u32
}

#[cfg(test)]
#[path = "decoder_tests.rs"]
mod tests;
