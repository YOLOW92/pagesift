//! 冒烟：载入模型目录的骨干，输入一张常数图，打印各段输出的形状和耗时。
//!     cargo run --profile quick -p pagesift-ocr --example backbone_smoke -- <模型目录> <高> <宽>
use std::time::Instant;
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::Path::new(&a[0]);
    let (h, w): (usize, usize) = (a[1].parse().unwrap(), a[2].parse().unwrap());
    let cfg: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("config.json")).unwrap()).unwrap();
    let wts = pagesift_infer::Weights::from_bytes(&std::fs::read(dir.join("model.safetensors")).unwrap()).unwrap();
    let c = pagesift_ocr::lcnet::Config::from_json(&cfg).unwrap();
    let b = pagesift_ocr::lcnet::Backbone::load(&wts, "model.backbone.encoder", &c).unwrap();
    let x = pagesift_infer::Tensor::new(vec![3, h, w], (0..3 * h * w).map(|i| ((i % 97) as f32 / 97.0) - 0.5).collect());
    let t = Instant::now();
    let outs = b.forward(&x);
    for o in &outs { println!("{:?} 均值 {:.5}", o.shape, o.data.iter().sum::<f32>() / o.data.len() as f32); }
    println!("{:.1} ms", t.elapsed().as_secs_f64() * 1e3);
}
