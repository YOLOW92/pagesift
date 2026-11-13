//! Single-process `pagesift-syntax` (`yolowsyntax`) parse + outline benchmark.
//!
//! Usage: `cargo run --release -p pagesift-syntax --example measure -- [rounds] <lang:path ...>`

#![forbid(unsafe_code)]

use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

fn infer_lang(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?;
    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "sh" | "bash" => "bash",
        "rb" => "ruby",
        "php" => "php",
        _ => return None,
    })
}

fn read_vm_hwm_kib() -> usize {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:")
            && let Some(num) = rest.split_whitespace().next()
            && let Ok(v) = num.parse::<usize>()
        {
            return v;
        }
    }
    0
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let rounds: usize = args
        .peek()
        .and_then(|s| s.parse::<usize>().ok())
        .map_or(25, |n| {
            args.next();
            n.max(1)
        });

    for spec in args {
        let (lang, path) = if let Some((l, p)) = spec.split_once(':') {
            (l, p)
        } else if let Some(l) = infer_lang(&spec) {
            (l, spec.as_str())
        } else {
            eprintln!("skip {spec}: unknown language");
            continue;
        };

        let src = std::fs::read_to_string(path).expect("read source file");
        let bytes = src.len();
        let warm = pagesift_syntax::outline(lang, &src);
        let (syms, secs, lossless) = match warm {
            Some(ref o) => {
                let covered: usize = o.sections.iter().map(|s| s.end - s.start).sum();
                (o.symbols.len(), o.sections.len(), covered == bytes)
            }
            None => (0, 0, true),
        };

        let mut times_ms = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let t0 = Instant::now();
            let out = pagesift_syntax::outline(lang, black_box(&src));
            black_box(&out);
            times_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        }
        times_ms.sort_by(f64::total_cmp);
        let median_ms = times_ms[times_ms.len() / 2];
        let mb_s = if median_ms > 0.0 {
            (bytes as f64 / 1e6) / (median_ms / 1e3)
        } else {
            0.0
        };
        let name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        println!(
            "{name}\t{lang}\t{bytes}\t{median_ms:.3}\t{mb_s:.2}\t{syms}\t{secs}\t{lossless}\t{}",
            read_vm_hwm_kib()
        );
    }
}
