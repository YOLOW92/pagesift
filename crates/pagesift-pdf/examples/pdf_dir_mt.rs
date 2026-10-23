//! Multi-threaded PDF batch converter for olmOCR-bench and stress benchmarks.
//! Uses all available CPU cores (up to 32 threads) to convert directories of PDFs.
//!
//! usage: pdf_dir_mt <src-dir> <out-dir> [num-threads]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "pdf") {
                out.push(p);
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let src = PathBuf::from(
        args.next()
            .expect("usage: pdf_dir_mt <src> <out> [threads]"),
    );
    let dst = PathBuf::from(
        args.next()
            .expect("usage: pdf_dir_mt <src> <out> [threads]"),
    );
    let num_threads = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(32, |n| n.get()));

    let mut files = Vec::new();
    walk(&src, &mut files);
    files.sort();

    let total_files = files.len();
    if total_files == 0 {
        eprintln!("No PDF files found in {}", src.display());
        return;
    }

    let files = Arc::new(files);
    let failed = Arc::new(AtomicUsize::new(0));
    let total_chars = Arc::new(AtomicUsize::new(0));
    let next_idx = Arc::new(AtomicUsize::new(0));

    let t0 = Instant::now();

    std::thread::scope(|s| {
        for _ in 0..num_threads {
            let files = Arc::clone(&files);
            let failed = Arc::clone(&failed);
            let total_chars = Arc::clone(&total_chars);
            let next_idx = Arc::clone(&next_idx);
            let src = src.clone();
            let dst = dst.clone();

            s.spawn(move || {
                loop {
                    let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                    if idx >= files.len() {
                        break;
                    }
                    let f = &files[idx];
                    let bytes = match std::fs::read(f) {
                        Ok(b) => b,
                        Err(e) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                            eprintln!("read error {}: {e}", f.display());
                            continue;
                        }
                    };

                    let md = match pagesift_pdf::extract(&bytes) {
                        Ok(doc) => doc.markdown,
                        Err(e) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                            eprintln!("extract error {}: {e}", f.display());
                            String::new()
                        }
                    };

                    total_chars.fetch_add(md.len(), Ordering::Relaxed);
                    if let Ok(rel) = f.strip_prefix(&src) {
                        let out_path = dst.join(format!(
                            "{}_pg1_repeat1.md",
                            rel.with_extension("").display()
                        ));
                        if let Some(parent) = out_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(out_path, md);
                    }
                }
            });
        }
    });

    let wall_secs = t0.elapsed().as_secs_f64();
    let fail_count = failed.load(Ordering::Relaxed);
    let chars = total_chars.load(Ordering::Relaxed);

    println!(
        "pagesift-mt: {} files across {} threads in {:.3} s = {:.1} pages/s ({:.2} ms/page), failed {}, markdown {:.2} MB",
        total_files,
        num_threads,
        wall_secs,
        total_files as f64 / wall_secs,
        (wall_secs * 1000.0) / total_files as f64,
        fail_count,
        chars as f64 / 1e6
    );
}
