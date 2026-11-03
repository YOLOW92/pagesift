//! 带 OCR 的批量转换：把目录（递归）里的每个 PDF 转成 Markdown，扫描页用 PP-OCRv6 识别；
//! 按 olmOCR-bench 的命名写出 `<输出目录>/<相对路径去掉 .pdf>_pg1_repeat1.md`。
//!
//!     pdf_ocr_dir <pdf 目录> <输出目录> <检测模型目录> <识别模型目录>
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use pagesift_ocr::Image;
use pagesift_ocr::pipeline::Engine;
use pagesift_pdf::{OcrLine, PageOcr};

struct Ocr(Engine);

impl std::fmt::Debug for Ocr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PP-OCRv6")
    }
}

impl PageOcr for Ocr {
    fn recognize(&self, width: u32, height: u32, gray: &[u8]) -> Result<Vec<OcrLine>, String> {
        let img = Image {
            width: width as usize,
            height: height as usize,
            channels: 1,
            data: gray.to_vec(),
        };
        let lines = self.0.run(&img)?;
        Ok(lines
            .into_iter()
            .map(|l| {
                let xs = l.quad.map(|p| p.0);
                let ys = l.quad.map(|p| p.1);
                let min = |v: [f32; 4]| v.into_iter().fold(f32::MAX, f32::min);
                let max = |v: [f32; 4]| v.into_iter().fold(f32::MIN, f32::max);
                OcrLine {
                    x0: min(xs),
                    y0: min(ys),
                    x1: max(xs),
                    y1: max(ys),
                    text: l.text,
                    score: l.score,
                }
            })
            .collect())
    }
}

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
    let a: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let (src, dst) = (&a[0], &a[1]);
    let engine = Engine::load(&a[2], &a[3]).expect("载入模型");
    // 按字节数估的时限没算 OCR 的耗时：关掉，只靠逐页的识别
    let opts = pagesift_pdf::Options {
        ocr: Some(Arc::new(Ocr(engine))),
        adaptive_timeout: false,
        ..Default::default()
    };
    let mut files = Vec::new();
    walk(src, &mut files);
    files.sort();
    let (mut total, mut failed) = (0.0, 0);
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let t = Instant::now();
        let md = match pagesift_pdf::extract_with(&bytes, &opts) {
            Ok(doc) => doc.markdown,
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", f.display());
                String::new()
            }
        };
        let s = t.elapsed().as_secs_f64();
        total += s;
        eprintln!("{}\t{:.0} ms", f.display(), s * 1e3);
        let rel = f.strip_prefix(src).unwrap().with_extension("");
        let out = dst.join(format!("{}_pg1_repeat1.md", rel.display()));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(out, md).unwrap();
    }
    println!(
        "pagesift+ocr: {} files, {failed} failed, {:.0} ms/file",
        files.len(),
        total * 1000.0 / files.len() as f64
    );
}
