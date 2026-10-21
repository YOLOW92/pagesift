//! 批量转换：把目录（递归）里的每个 PDF 转成 Markdown，按 olmOCR-bench 的命名写出
//! `<输出目录>/<相对路径去掉 .pdf>_pg1_repeat1.md`，打印每个文件的平均耗时。
//! `pdf_dir <pdf 目录> <输出目录>`
use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "pdf") {
            out.push(p);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let src = PathBuf::from(args.next().expect("usage: pdf_dir <src> <out>"));
    let dst = PathBuf::from(args.next().expect("usage: pdf_dir <src> <out>"));
    let mut files = Vec::new();
    walk(&src, &mut files);
    files.sort();
    let (mut total, mut failed) = (0.0, 0);
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let t = Instant::now();
        let md = match pagesift_pdf::extract(&bytes) {
            Ok(doc) => doc.markdown,
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", f.display());
                String::new()
            }
        };
        total += t.elapsed().as_secs_f64();
        let rel = f.strip_prefix(&src).unwrap().with_extension("");
        let out = dst.join(format!("{}_pg1_repeat1.md", rel.display()));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(out, md).unwrap();
    }
    println!(
        "pagesift: {} files, {failed} failed, {:.2} ms/file",
        files.len(),
        total * 1000.0 / files.len() as f64
    );
}
