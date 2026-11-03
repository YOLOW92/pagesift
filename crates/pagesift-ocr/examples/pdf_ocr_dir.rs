//! 带 OCR 的批量转换：把目录（递归）里的每个 PDF 转成 Markdown，扫描页用 PP-OCRv6 识别；
//! 按 olmOCR-bench 的命名写出 `<输出目录>/<相对路径去掉 .pdf>_pg1_repeat1.md`。
//!
//!     pdf_ocr_dir <pdf 目录> <输出目录> <检测模型目录> <识别模型目录> [<版面模型目录> <公式模型目录>]
//!
//! 环境变量：`OCR_DEVICE=cpu|gpu`（默认 cpu）、`OCR_JOBS=N`（同时处理的 PDF 数，默认 1）。
//! 结尾打印实际用的设备、显存峰值和平均批大小。
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pagesift_ocr::device::Device;
use pagesift_ocr::pdf::PdfOcr;

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
    let math = (a.len() >= 6).then(|| (a[4].clone(), a[5].clone()));
    let device = match std::env::var("OCR_DEVICE").as_deref() {
        Ok("gpu") => Device::Gpu { index: 0 },
        _ => Device::Cpu,
    };
    let jobs: usize = std::env::var("OCR_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let engine =
        Arc::new(PdfOcr::from_dirs(a[2].clone(), a[3].clone(), math, device).expect("载入模型"));
    // 按字节数估的时限没算 OCR 的耗时：关掉，只靠逐页的识别
    let opts = pagesift_pdf::Options {
        ocr: Some(engine.clone()),
        adaptive_timeout: false,
        // 所有带图的页都识别（Firecrawl 的 mode: "ocr"）：MRC 页有文字层，不判为扫描页
        force_ocr: std::env::var_os("PDF_OCR_FORCE").is_some(),
        ..Default::default()
    };
    let mut files = Vec::new();
    walk(src, &mut files);
    files.sort();
    let next = AtomicUsize::new(0);
    let totals = Mutex::new((0.0f64, 0usize));
    let wall = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                while let Some(f) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let bytes = std::fs::read(f).unwrap();
                    let t = Instant::now();
                    let (md, bad) = match pagesift_pdf::extract_with(&bytes, &opts) {
                        Ok(doc) => (doc.markdown, 0),
                        Err(e) => {
                            eprintln!("{}: {e}", f.display());
                            (String::new(), 1)
                        }
                    };
                    let secs = t.elapsed().as_secs_f64();
                    {
                        let mut g = totals.lock().unwrap();
                        g.0 += secs;
                        g.1 += bad;
                    }
                    eprintln!("{}\t{:.0} ms", f.display(), secs * 1e3);
                    let rel = f.strip_prefix(src).unwrap().with_extension("");
                    let out = dst.join(format!("{}_pg1_repeat1.md", rel.display()));
                    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
                    std::fs::write(out, md).unwrap();
                }
            });
        }
    });
    let (total, failed) = *totals.lock().unwrap();
    let wall = wall.elapsed().as_secs_f64();
    println!(
        "pagesift+ocr: {} files, {failed} failed, {:.0} ms/file, wall {:.1} s, {:.2} pages/s",
        files.len(),
        total * 1000.0 / files.len() as f64,
        wall,
        files.len() as f64 / wall
    );
    let Some(e) = engine.engine() else { return };
    let st = e.device_status();
    println!("device: {:?}, fallback: {:?}", st.actual, st.fallback);
    if let Some(g) = e.gpu_stats() {
        println!(
            "gpu: peak {} MiB, needed {} MiB, {} batches, {} lines, {:.1} lines/batch, {} splits",
            g.peak_bytes >> 20,
            g.needed_bytes >> 20,
            g.batches,
            g.lines,
            g.lines as f64 / g.batches.max(1) as f64,
            g.splits
        );
    }
}
