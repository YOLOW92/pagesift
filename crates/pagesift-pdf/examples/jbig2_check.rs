//! 对照用：把目录里每个 .jb2e（pdfimages -jbig2 的输出：嵌入码流，全局段在同名 .jb2g）
//! 解成裸灰度，供 benchmarks 里的脚本和参照解码比较。
//!
//!     cargo run --profile quick -p pagesift-pdf --example jbig2_check -- <目录> <输出目录>
//!
//! 每个 <名字>.jb2e 写出 <名字>.y（每像素一字节，黑 0 白 255），标准输出每行：名字、宽、高、
//! 解码毫秒数；解不开的打印错误。

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (src, out) = (args.next().expect("目录"), args.next().expect("输出目录"));
    std::fs::create_dir_all(&out).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&src)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jb2e"))
        .collect();
    files.sort();
    for p in files {
        let data = std::fs::read(&p).unwrap();
        let globals = std::fs::read(p.with_extension("jb2g")).ok();
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let t = Instant::now();
        let res = pagesift_pdf::fuzzing::decode_jbig2(&data, globals.as_deref());
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
