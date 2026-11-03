//! 用随机权重的小模型测缓存和生成逻辑；真实权重的流程检查标了 `#[ignore]`（要读 727 MB）。

use serde_json::json;

use super::*;

const D: usize = 8;
const HEADS: usize = 2;
const LAYERS: usize = 2;
const FFN: usize = 16;
const VOCAB: usize = 20;
const POSITIONS: usize = 32;
const EOS: usize = 2;

/// xorshift 生成 [−1, 1) 的数，固定种子。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }

    fn fill(&mut self, n: usize) -> Vec<f32> {
        (0..n).map(|_| self.next()).collect()
    }
}

/// 把张量写成 safetensors 字节，走真实的权重读取路径。
fn safetensors(tensors: &[(String, Vec<usize>, Vec<f32>)]) -> Vec<u8> {
    let mut head = serde_json::Map::new();
    let mut data = Vec::new();
    for (name, shape, values) in tensors {
        let start = data.len();
        for v in values {
            data.extend_from_slice(&v.to_le_bytes());
        }
        head.insert(
            name.clone(),
            json!({"dtype": "F32", "shape": shape, "data_offsets": [start, data.len()]}),
        );
    }
    let head = serde_json::to_vec(&head).unwrap();
    let mut out = (head.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(&head);
    out.extend_from_slice(&data);
    out
}

fn tiny_tensors(seed: u64) -> Vec<(String, Vec<usize>, Vec<f32>)> {
    let mut r = Rng(seed);
    let mut t = Vec::new();
    let mut add = |name: String, shape: Vec<usize>, r: &mut Rng| {
        let n = shape.iter().product();
        t.push((name, shape, r.fill(n)));
    };
    add(
        "model.decoder.embed_tokens.weight".into(),
        vec![VOCAB, D],
        &mut r,
    );
    add(
        "model.decoder.embed_positions.weight".into(),
        vec![POSITIONS + POSITION_OFFSET, D],
        &mut r,
    );
    add("lm_head.weight".into(), vec![VOCAB, D], &mut r);
    let norms = [
        "model.decoder.layernorm_embedding".to_string(),
        "model.decoder.layer_norm".to_string(),
    ];
    let mut norm_names: Vec<String> = norms.to_vec();
    for l in 0..LAYERS {
        let p = format!("model.decoder.layers.{l}");
        for attn in ["self_attn", "encoder_attn"] {
            for proj in ["q_proj", "k_proj", "v_proj", "out_proj"] {
                add(format!("{p}.{attn}.{proj}.weight"), vec![D, D], &mut r);
                add(format!("{p}.{attn}.{proj}.bias"), vec![D], &mut r);
            }
        }
        add(format!("{p}.fc1.weight"), vec![FFN, D], &mut r);
        add(format!("{p}.fc1.bias"), vec![FFN], &mut r);
        add(format!("{p}.fc2.weight"), vec![D, FFN], &mut r);
        add(format!("{p}.fc2.bias"), vec![D], &mut r);
        for n in [
            "self_attn_layer_norm",
            "encoder_attn_layer_norm",
            "final_layer_norm",
        ] {
            norm_names.push(format!("{p}.{n}"));
        }
    }
    for n in norm_names {
        add(format!("{n}.weight"), vec![D], &mut r);
        add(format!("{n}.bias"), vec![D], &mut r);
    }
    t
}

fn build(tensors: &[(String, Vec<usize>, Vec<f32>)], max_length: usize) -> Decoder {
    let w = Weights::from_bytes(&safetensors(tensors)).unwrap();
    let text = json!({
        "d_model": D, "decoder_attention_heads": HEADS, "decoder_layers": LAYERS,
        "activation_function": "gelu", "scale_embedding": true
    });
    let generation = json!({
        "decoder_start_token_id": EOS, "eos_token_id": EOS, "forced_eos_token_id": EOS,
        "max_length": max_length
    });
    Decoder::from_weights(&w, &text, &generation).unwrap()
}

fn tiny(seed: u64) -> Decoder {
    build(&tiny_tensors(seed), POSITIONS)
}

fn memory(seed: u64, l: usize) -> Tensor {
    Tensor::new(vec![l, D], Rng(seed).fill(l * D))
}

/// 每步全量重算：新缓存（交叉 K、V 重算）、喂入整个前缀，取最后一行。
fn full_logits(dec: &Decoder, mem: &Tensor, prefix: &[u32]) -> Vec<f32> {
    let mut c = dec.new_cache(mem).unwrap();
    let h = dec.forward(prefix, &mut c).unwrap();
    dec.logits(&h).unwrap()
}

#[test]
fn cached_steps_match_full_recompute() {
    for seed in [1u64, 7, 12345] {
        let dec = tiny(seed);
        let mem = memory(seed + 1, 9);
        let mut cache = dec.new_cache(&mem).unwrap();
        let mut seq = vec![dec.start];
        for step in 0..POSITIONS - 1 {
            let h = dec.forward(&[*seq.last().unwrap()], &mut cache).unwrap();
            let a = dec.logits(&h).unwrap();
            let b = full_logits(&dec, &mem, &seq);
            for (x, y) in a.iter().zip(&b) {
                assert!((x - y).abs() < 1e-4, "第 {step} 步：{x} 对 {y}");
            }
            assert_eq!(argmax(&a), argmax(&b), "第 {step} 步选的 token 不同");
            seq.push(argmax(&a));
        }
    }
}

/// 因果性：改动后面的 token 不影响前面位置的隐状态。
#[test]
fn self_attention_is_causal() {
    let dec = tiny(3);
    let mem = memory(4, 5);
    let run = |ids: &[u32]| {
        let mut c = dec.new_cache(&mem).unwrap();
        dec.forward(ids, &mut c).unwrap()
    };
    let (a, b) = (run(&[2, 5, 6, 7]), run(&[2, 5, 6, 8]));
    assert_eq!(a.data[..3 * D], b.data[..3 * D]);
    assert_ne!(a.data[3 * D..], b.data[3 * D..]);
}

#[test]
fn generate_matches_naive_greedy() {
    let dec = tiny(11);
    let mem = memory(12, 7);
    let got = dec.generate(&mem, POSITIONS).unwrap();
    let mut seq = vec![dec.start];
    let mut want = Vec::new();
    while seq.len() < POSITIONS {
        let next = if seq.len() + 1 == POSITIONS {
            dec.eos
        } else {
            argmax(&full_logits(&dec, &mem, &seq))
        };
        if next == dec.eos {
            break;
        }
        want.push(next);
        seq.push(next);
    }
    assert_eq!(got, want);
}

/// 第一个 token 用位置表的第 2 行（偏移 2），前两行不起作用。
#[test]
fn position_offset_is_two() {
    let base = tiny_tensors(21);
    let mem = memory(22, 4);
    let logits = |tensors: &[(String, Vec<usize>, Vec<f32>)]| {
        let dec = build(tensors, POSITIONS);
        full_logits(&dec, &mem, &[2])
    };
    let perturb = |row: usize| {
        let mut t = base.clone();
        let e = t
            .iter_mut()
            .find(|e| e.0 == "model.decoder.embed_positions.weight")
            .unwrap();
        for v in &mut e.2[row * D..(row + 1) * D] {
            *v += 1.0;
        }
        t
    };
    let plain = logits(&base);
    assert_eq!(plain, logits(&perturb(0)));
    assert_eq!(plain, logits(&perturb(1)));
    assert_ne!(plain, logits(&perturb(POSITION_OFFSET)));
}

/// 给 lm_head 加一个很大的偏置，让某个 token 必胜。
fn biased(seed: u64, token: usize) -> Decoder {
    let mut dec = tiny(seed);
    let mut bias = vec![0f32; VOCAB];
    bias[token] = 1e6;
    dec.lm_head.bias = Some(bias);
    dec
}

#[test]
fn stops_at_eos() {
    let dec = biased(31, EOS);
    assert!(dec.generate(&memory(32, 4), POSITIONS).unwrap().is_empty());
}

#[test]
fn forces_eos_at_max_length() {
    let dec = biased(33, 5);
    let mem = memory(34, 4);
    // 序列总长 6 = 起始 + 4 个 token + 强制的 eos
    assert_eq!(dec.generate(&mem, 6).unwrap(), vec![5; 4]);
    // 表长封顶：不会越界
    assert_eq!(dec.generate(&mem, usize::MAX).unwrap().len(), POSITIONS - 2);
    assert!(dec.generate(&mem, 1).unwrap().is_empty());
    assert!(dec.generate(&mem, 0).unwrap().is_empty());
}

#[test]
fn rejects_bad_input() {
    let dec = tiny(41);
    assert!(dec.generate(&Tensor::zeros(vec![4, D + 1]), 8).is_err());
    assert!(dec.generate(&Tensor::zeros(vec![0, D]), 8).is_err());
    let nan = Tensor::new(vec![1, D], vec![f32::NAN; D]);
    assert!(dec.generate(&nan, 8).is_err());
    let mut c = dec.new_cache(&memory(42, 2)).unwrap();
    assert!(dec.forward(&[VOCAB as u32], &mut c).is_err());
}

/// 真实权重 + 随机 memory：流程能跑、无 NaN、能停。用
/// `cargo test --profile quick -p pagesift-ocr real_weights -- --ignored --nocapture` 跑。
#[test]
#[ignore = "要读 727 MB 权重"]
fn real_weights_random_memory() {
    let dir = crate::models::cache_root()
        .unwrap()
        .join("PP-FormulaNet_plus-L_safetensors");
    let dec = Decoder::load(&dir).unwrap();
    assert_eq!(dec.max_length(), 1537);
    let mut r = Rng(99);
    let mem = Tensor::new(vec![144, 512], r.fill(144 * 512));
    let t = std::time::Instant::now();
    let ids = dec.generate(&mem, 64).unwrap();
    println!(
        "{} 个 token，{:.1} ms/token",
        ids.len(),
        t.elapsed().as_secs_f64() * 1e3 / (ids.len() + 1) as f64
    );
    assert!(ids.len() <= 62);
    assert!(!ids.contains(&dec.eos));
}
