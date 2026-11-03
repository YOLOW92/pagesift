//! 检测检查：载入检测模型，逐张读裸像素图跑检测，把框写成文本（每行 8 个数：四角 x y 和分数）。
//!     cargo run --profile release-fast -p pagesift-ocr --example det_check -- \
//!         <模型目录> <输出目录> <裸图1> <宽1> <高1> [<裸图2> <宽2> <高2> ...]
//! 裸图是按行存放、无行间填充的 8 位像素，灰度或 RGB（由文件大小判断）。
//! 输出 `<输出目录>/<裸图文件名>.boxes.txt`，耗时打到标准错误。
use std::io::Write;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 5 || !(a.len() - 2).is_multiple_of(3) {
        eprintln!("用法：det_check <模型目录> <输出目录> <裸图> <宽> <高> ...");
        std::process::exit(2);
    }
    let t = Instant::now();
    let det = pagesift_ocr::det::Det::load(std::path::Path::new(&a[0])).unwrap();
    eprintln!("载入 {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let out_dir = std::path::Path::new(&a[1]);
    for c in a[2..].chunks(3) {
        let data = std::fs::read(&c[0]).unwrap();
        let (w, h): (usize, usize) = (c[1].parse().unwrap(), c[2].parse().unwrap());
        let channels = data.len() / (w * h);
        assert!(channels == 1 || channels == 3, "文件大小与宽高不符");
        let img = pagesift_ocr::Image {
            width: w,
            height: h,
            channels,
            data,
        };
        let name = std::path::Path::new(&c[0])
            .file_name()
            .unwrap()
            .to_string_lossy();
        let t = Instant::now();
        let (prob, pw, ph) = det.prob_map(&img).unwrap();
        let net_ms = t.elapsed().as_secs_f64() * 1e3;
        let boxes = det.boxes_from_prob(&prob, pw, ph, w, h);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("{name}: 网络 {net_ms:.0} ms，后处理 {:.0} ms", ms - net_ms);
        // 设了 DET_DUMP_PROB 就把概率图另存成 8 位 PGM
        if std::env::var_os("DET_DUMP_PROB").is_some() {
            let mut pgm = format!("P5\n{pw} {ph}\n255\n").into_bytes();
            pgm.extend(prob.iter().map(|v| (v * 255.0).round() as u8));
            std::fs::write(out_dir.join(format!("{name}.prob.pgm")), pgm).unwrap();
        }
        let mut f = std::fs::File::create(out_dir.join(format!("{name}.boxes.txt"))).unwrap();
        for (q, s) in &boxes {
            for (x, y) in q {
                write!(f, "{x} {y} ").unwrap();
            }
            writeln!(f, "{s:.4}").unwrap();
        }
        eprintln!("{name}: {w}×{h}，{} 个框，{ms:.0} ms", boxes.len());
    }
}
