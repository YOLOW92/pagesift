//! 文档转换的进程内耗时：每个文件先转一次预热，再转 N 次取中位数（毫秒，越低越好）。
//! 用的是引擎同一个函数 `pagesift::office_markdown`（EPUB 走同一个 HTML 提取器）。
//!
//! 用法：cargo run --release -p pagesift --example office_bench -- <次数> <文件...>
//! 输出 TSV：文件名、中位耗时 ms、Markdown 字节数。

use std::time::Instant;

/// 和发布的可执行程序一样的全局分配器，量出来的才是用户拿到的。
#[global_allocator]
static ALLOC: pagesift_sys::SmallCache = pagesift_sys::SmallCache;

fn main() {
    let mut args = std::env::args().skip(1);
    let runs: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(5);
    for path in args {
        let bytes = std::fs::read(&path).expect("read");
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
        let Some(format) = pagesift_office::Format::detect(&bytes, Some(&name), None)
            .or_else(|| pagesift_office::Format::from_extension(ext))
        else {
            eprintln!("skip {name}");
            continue;
        };
        let out = pagesift::office_markdown(&bytes, format, None);
        let size = out.as_ref().map_or(0, |d| d.markdown.len());
        let mut times: Vec<f64> = (0..runs)
            .map(|_| {
                let t = Instant::now();
                let r = pagesift::office_markdown(&bytes, format, None);
                std::hint::black_box(&r);
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("{name}\t{:.3}\t{size}", times[times.len() / 2]);
    }
}
