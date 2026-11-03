//! 对照用：把目录里每个 .jpg 解出来写成裸像素，供 benchmarks 里的脚本和参照解码器比较。
//!
//!     cargo run --profile quick -p pagesift-pdf --example jpeg_check -- <jpg 目录> <输出目录>
//!
//! 每个 <名字>.jpg 写出 <名字>.y：走 OCR 用的灰度直出（YCbCr 图就是 Y 通道），
//! 标准输出每行：名字、宽、高、原分量数、解码毫秒数；解不开的打印错误。

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (src, out) = (
        args.next().expect("jpg 目录"),
        args.next().expect("输出目录"),
    );
    std::fs::create_dir_all(&out).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&src)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
        .collect();
    files.sort();
    for p in files {
        let data = std::fs::read(&p).unwrap();
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        // 原分量数从帧头读（SOFn 段第 10 字节），只为输出里标注
        let comps = data
            .windows(2)
            .position(|w| w[0] == 0xFF && matches!(w[1], 0xC0..=0xC2))
            .and_then(|i| data.get(i + 9).copied())
            .unwrap_or(0);
        let t = Instant::now();
        let res = pagesift_pdf::fuzzing::decode_jpeg(&data, None, true)
            .map(|(w, h, _, px)| (w, h, comps, px));
        let ms = t.elapsed().as_secs_f64() * 1e3;
        match res {
            Ok((w, h, c, px)) => {
                std::fs::write(format!("{out}/{name}.y"), px).unwrap();
                println!("{name}\t{w}\t{h}\t{c}\t{ms:.1}");
            }
            Err(e) => println!("{name}\tERROR\t{e}"),
        }
    }
}
