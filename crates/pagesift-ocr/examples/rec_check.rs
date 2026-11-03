//! 识别检查：载入识别模型，逐个读 PPM（P6，8 位 RGB）行图，打印识别文字、置信度和耗时。
//!     cargo run --profile release-fast -p pagesift-ocr --example rec_check -- <模型目录> <行图.ppm>...
use std::time::Instant;

/// 最小的 P6 PPM 读取：头是 "P6 宽 高 255"（空白分隔，可有 # 注释），之后紧跟 RGB 数据。
fn read_ppm(path: &str) -> pagesift_ocr::Image {
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
    pagesift_ocr::Image {
        width: w,
        height: h,
        channels: 3,
        data,
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let t = Instant::now();
    let rec = pagesift_ocr::rec::Rec::load(std::path::Path::new(&a[0])).unwrap();
    println!("载入 {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    for p in &a[1..] {
        let img = read_ppm(p);
        let t = Instant::now();
        let (text, score) = rec.recognize(&img).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let name = std::path::Path::new(p)
            .file_name()
            .unwrap()
            .to_string_lossy();
        println!(
            "{name}\t{}x{}\t{ms:.0} ms\t{score:.3}\t{text}",
            img.width, img.height
        );
    }
}
