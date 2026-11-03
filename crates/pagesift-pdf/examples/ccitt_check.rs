//! 对照用：把目录里每个 .ccitt（pdfimages -ccitt 的输出：原码流加同名 .params）解成裸灰度，
//! 供 benchmarks 里的脚本和参照解码比较。
//!
//!     cargo run --profile quick -p pagesift-pdf --example ccitt_check -- <目录> <输出目录>
//!
//! 每个 <名字>.ccitt 写出 <名字>.y（每像素一字节，黑 0 白 255），标准输出每行：名字、宽、高、
//! 解码毫秒数；解不开的打印错误。.params 认 -1/-2/-4（K = 0、>0、<0）、-A（字节对齐）、
//! -X 宽、-H 行数（缺省 0：解到数据末尾）、-W（BlackIs1）。

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (src, out) = (args.next().expect("目录"), args.next().expect("输出目录"));
    std::fs::create_dir_all(&out).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&src)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ccitt"))
        .collect();
    files.sort();
    for p in files {
        let data = std::fs::read(&p).unwrap();
        let params = std::fs::read_to_string(p.with_extension("params")).unwrap();
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let (mut k, mut align, mut cols, mut rows, mut black1) = (0i64, false, 0u32, 0u32, false);
        let mut it = params.split_whitespace();
        while let Some(a) = it.next() {
            match a {
                "-1" => k = 0,
                "-2" => k = 1,
                "-4" => k = -1,
                "-A" => align = true,
                "-W" => black1 = true,
                "-X" => cols = it.next().unwrap().parse().unwrap(),
                "-H" => rows = it.next().unwrap().parse().unwrap(),
                _ => {}
            }
        }
        let t = Instant::now();
        let res = pagesift_pdf::fuzzing::decode_ccitt(&data, k, cols, rows, black1, align);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        match res {
            Ok((w, h, px)) => {
                std::fs::write(format!("{out}/{name}.y"), px).unwrap();
                println!("{name}\t{w}\t{h}\t{ms:.2}");
            }
            Err(e) => println!("{name}\tERROR\t{e}"),
        }
    }
}
