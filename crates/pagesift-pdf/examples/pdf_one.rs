//! 单个 PDF 的转换耗时：`pdf_one <file.pdf> [runs]`，打印最快一次的毫秒数和输出长度。
//! 峰值内存用外部工具量（如 `/usr/bin/time -f %M`），所以每个文件单独起一个进程。
use std::time::Instant;

/// 和发布的可执行程序一样的全局分配器，量出来的才是用户拿到的。
#[global_allocator]
static ALLOC: pagesift_sys::SmallCache = pagesift_sys::SmallCache;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: pdf_one <file.pdf> [runs]");
    let runs: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);
    let bytes = std::fs::read(&path).expect("read");
    let mut best = f64::MAX;
    let mut chars = 0;
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        let md = pagesift_pdf::extract(&bytes)
            .map(|d| d.markdown)
            .unwrap_or_default();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        chars = md.chars().count();
    }
    println!("{path}\t{best:.2}\t{chars}");
}
