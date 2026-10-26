//! 逐份输出完整转换结果，用来对比两版实现是否逐字一致：
//! `pdf_snap <文件清单> <输出目录>`，清单每行一个 PDF 路径；第 i 行的结果写到 `<输出目录>/<i>.txt`，
//! 内容是 `PdfDocument` 的 Debug 输出，报错时只写错误种类（错误信息随实现变化，不比）。
//! 另在标准输出打印每份的耗时（毫秒，单次）。
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let list = args.next().expect("usage: pdf_snap <list> <out-dir>");
    let out = std::path::PathBuf::from(args.next().expect("usage: pdf_snap <list> <out-dir>"));
    std::fs::create_dir_all(&out).expect("mkdir");
    let paths = std::fs::read_to_string(list).expect("read list");
    for (i, path) in paths.lines().filter(|l| !l.is_empty()).enumerate() {
        let bytes = std::fs::read(path).expect("read pdf");
        let t = Instant::now();
        let result = pagesift_pdf::extract(&bytes);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let text = match result {
            Ok(doc) => format!("{path}\n{doc:#?}\n"),
            Err(e) => {
                let kind = match e {
                    pagesift_pdf::Error::Malformed(_) => "Malformed",
                    pagesift_pdf::Error::Encrypted => "Encrypted",
                    pagesift_pdf::Error::TooLarge(w) => w,
                    pagesift_pdf::Error::Timeout => "Timeout",
                    _ => "Other",
                };
                format!("{path}\nERR {kind}\n")
            }
        };
        std::fs::write(out.join(format!("{i}.txt")), text).expect("write");
        println!("{i}\t{ms:.2}\t{path}");
    }
}
