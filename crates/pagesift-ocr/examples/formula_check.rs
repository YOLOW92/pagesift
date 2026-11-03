//! 公式识别端到端检查：载入编码器和解码器（共用一次读权重），逐个读 PPM（P6，8 位 RGB）公式图，
//! 打印 LaTeX、token 数和耗时。不给图时用随机 memory 跑一遍解码器（流程、NaN、停止）。
//!     cargo run --profile release-fast -p pagesift-ocr --example formula_check -- <模型目录> [公式图.ppm]...
use std::path::Path;
use std::time::Instant;

use pagesift_infer::{Tensor, Weights};
use pagesift_ocr::formula::{Decoder, Tokenizer, to_latex};
use pagesift_ocr::formula_enc::{Config, Encoder, preprocess};

/// 最小的 P6 PPM 读取：头是 "P6 宽 高 255"（空白分隔），之后紧跟 RGB 数据。
fn read_ppm(path: &str) -> pagesift_ocr::Image {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("读 {path} 失败：{e}"));
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while b[i].is_ascii_whitespace() {
            i += 1;
        }
        let s = i;
        while !b[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&b[s..i]).into_owned());
    }
    assert_eq!(fields[0], "P6", "{path} 不是 P6 PPM");
    let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    pagesift_ocr::Image {
        width: w,
        height: h,
        channels: 3,
        data: b[i + 1..i + 1 + w * h * 3].to_vec(),
    }
}

fn json(dir: &Path, name: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = Path::new(&a[0]);
    let t = Instant::now();
    let cfg = json(dir, "config.json");
    let w = Weights::from_bytes(&std::fs::read(dir.join("model.safetensors")).unwrap()).unwrap();
    let dec = Decoder::from_weights(
        &w,
        &cfg["text_config"],
        &json(dir, "generation_config.json"),
    )
    .unwrap();
    let enc = Encoder::from_weights(&w, Config::from_json(&cfg).unwrap()).unwrap();
    drop(w);
    let tok = Tokenizer::load(dir).unwrap();
    println!("载入 {:.0} ms", t.elapsed().as_secs_f64() * 1e3);

    if a.len() == 1 {
        // 固定种子的随机 memory
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let data = (0..144 * 512)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 40) as f32 / (1u64 << 23) as f32 - 1.0
            })
            .collect();
        let mem = Tensor::new(vec![144, 512], data);
        let t = Instant::now();
        let ids = dec.generate(&mem, 200).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "随机 memory：{} 个 token，{ms:.0} ms，{:.1} ms/token",
            ids.len(),
            ms / (ids.len() + 1) as f64
        );
        println!("{}", to_latex(&tok.decode(&ids)));
        return;
    }

    for p in &a[1..] {
        let img = read_ppm(p);
        let t = Instant::now();
        let x = preprocess(&img);
        let t_pre = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let mem = enc.encode(&x).unwrap();
        let t_enc = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let ids = dec.generate(&mem, dec.max_length()).unwrap();
        let t_dec = t.elapsed().as_secs_f64() * 1e3;
        let name = Path::new(p).file_name().unwrap().to_string_lossy();
        println!(
            "{name}\t预处理 {t_pre:.0} ms\t编码 {t_enc:.0} ms\t解码 {t_dec:.0} ms（{} 个 token，{:.1} ms/token）",
            ids.len(),
            t_dec / (ids.len() + 1) as f64
        );
        println!("  原始：{}", tok.decode(&ids));
        println!("  LaTeX：{}", to_latex(&tok.decode(&ids)));
    }
    // 内存峰值（Linux 才有这个文件）
    if let Ok(s) = std::fs::read_to_string("/proc/self/status")
        && let Some(l) = s.lines().find(|l| l.starts_with("VmHWM"))
    {
        println!("{l}");
    }
}
