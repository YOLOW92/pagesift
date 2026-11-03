//! 公式编码器检查：载入模型，对一张图（P6 PPM；不给图就用合成的公式状图）跑一次编码，
//! 逐段打印形状、min/max/mean、NaN/Inf 个数和耗时。
//!     cargo run --profile release-fast -p pagesift-ocr --example formula_enc_check -- <模型目录> [图.ppm]
use std::time::Instant;

use pagesift_infer::Tensor;
use pagesift_ocr::Image;
use pagesift_ocr::formula_enc::{Encoder, preprocess};

/// 最小的 P6 PPM 读取：头是 "P6 宽 高 255"（空白分隔，可有 # 注释），之后紧跟 RGB 数据。
fn read_ppm(path: &str) -> Image {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("读 {path} 失败：{e}"));
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i] == b'#' {
            while b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let s = i;
        while !b[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&b[s..i]).into_owned());
    }
    assert_eq!(fields[0], "P6", "{path} 不是 P6 PPM");
    assert_eq!(fields[3], "255", "{path} 不是 8 位");
    let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let data = b[i + 1..i + 1 + w * h * 3].to_vec();
    Image {
        width: w,
        height: h,
        channels: 3,
        data,
    }
}

/// 合成图：白底上几条黑色横竖笔画，宽 400、高 100。没有真实公式图时看数值范围用。
fn synthetic() -> Image {
    let (w, h) = (400, 100);
    let mut data = vec![255u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let bar = (20..380).contains(&x) && (48..52).contains(&y);
            let stem = x % 60 < 4 && (15..85).contains(&y) && (20..380).contains(&x);
            if bar || stem {
                data[y * w + x] = 0;
            }
        }
    }
    Image {
        width: w,
        height: h,
        channels: 1,
        data,
    }
}

fn stats(name: &str, t: &Tensor, ms: f64) {
    let bad = t.data.iter().filter(|v| !v.is_finite()).count();
    let (mut lo, mut hi, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0f64);
    for &v in t.data.iter().filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
        sum += f64::from(v);
    }
    let mean = sum / t.data.len().max(1) as f64;
    println!(
        "{name:<16} {:<16} min {lo:>10.4} max {hi:>10.4} mean {mean:>9.4} 非有限 {bad:<6} {ms:>8.1} ms",
        format!("{:?}", t.shape)
    );
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let t = Instant::now();
    let enc = Encoder::load(std::path::Path::new(&a[0])).unwrap();
    println!("载入 {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let img = a.get(1).map_or_else(synthetic, |p| read_ppm(p));
    let t = Instant::now();
    let x = preprocess(&img);
    let pre_ms = t.elapsed().as_secs_f64() * 1e3;
    stats("preprocess", &x, pre_ms);
    let t0 = Instant::now();
    let mut last = t0;
    let m = enc
        .encode_traced(&x, |name, t| {
            stats(name, t, last.elapsed().as_secs_f64() * 1e3);
            last = Instant::now();
        })
        .unwrap();
    println!(
        "编码合计 {:.0} ms，输出 {:?}",
        t0.elapsed().as_secs_f64() * 1e3,
        m.shape
    );
    // 不带逐段统计再跑一次，得到干净的耗时
    let t = Instant::now();
    let m2 = enc.encode(&x).unwrap();
    println!("第二次编码 {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    assert_eq!(m.data, m2.data, "两次编码结果应逐位相同");
}
