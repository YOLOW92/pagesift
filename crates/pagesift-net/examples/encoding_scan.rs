//! 编码检测批处理：读一个文件清单（每行一个路径），输出 `路径<TAB>编码`。
//! 用途：D6 编码识别基准，以及 0.50/0.70 同机对比。
//!
//! usage: encoding_scan <list.txt> <out.tsv> [threads]

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let list = args
        .next()
        .ok_or("usage: encoding_scan <list.txt> <out.tsv> [threads]")?;
    let out = args
        .next()
        .ok_or("usage: encoding_scan <list.txt> <out.tsv> [threads]")?;
    let threads: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16);

    let paths: Vec<String> = BufReader::new(File::open(&list)?)
        .lines()
        .collect::<Result<_, _>>()?;
    let n = paths.len();
    let paths = Arc::new(paths);
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(std::io::BufWriter::new(File::create(&out)?)));
    let start = Instant::now();

    std::thread::scope(|scope| {
        for _ in 0..threads.min(n.max(1)) {
            let paths = Arc::clone(&paths);
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let p = &paths[i];
                    let enc = match std::fs::read(p) {
                        Ok(bytes) => {
                            let (_, name, _, _) = pagesift_net::detect_charset(&bytes, None, None);
                            name.to_string()
                        }
                        Err(_) => "error".to_string(),
                    };
                    let mut w = results.lock().expect("lock");
                    let _ = writeln!(w, "{p}\t{enc}");
                }
            });
        }
    });

    let wall = start.elapsed().as_secs_f64();
    eprintln!(
        "files={n} threads={} wall_s={:.3} files_per_s={:.1}",
        threads,
        wall,
        n as f64 / wall
    );
    Ok(())
}
