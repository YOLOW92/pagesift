//! 单进程测量（不含文件读取），使用者负责与其他评测串行运行。
use pagesift_compress::{compress, decompress};
use std::hint::black_box;
use std::time::{Duration, Instant};

fn measure(mut f: impl FnMut()) -> f64 {
    f();
    let mut times = Vec::new();
    let start = Instant::now();
    while times.len() < 5 || start.elapsed() < Duration::from_millis(250) {
        let t = Instant::now();
        f();
        times.push(t.elapsed().as_secs_f64());
    }
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

fn main() {
    let levels: Vec<i32> = std::env::var("PAGESIFT_COMPRESS_LEVELS").map_or_else(
        |_| vec![3, 9, 19],
        |s| s.split(',').map(|v| v.parse().expect("整数等级")).collect(),
    );
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("读取样本");
        if path.ends_with(".zst") {
            let raw = decompress(&data, usize::MAX).expect("解压样本");
            let t = measure(|| {
                black_box(decompress(black_box(&data), usize::MAX).unwrap());
            });
            println!("{path} decode {:.1} MB/s", raw.len() as f64 / t / 1e6);
            continue;
        }
        for &level in &levels {
            let z = compress(&data, level);
            assert_eq!(decompress(&z, data.len()).unwrap(), data);
            let t = measure(|| {
                black_box(compress(black_box(&data), level));
            });
            println!(
                "{path} level {level}: {} bytes, {:.1} MB/s",
                z.len(),
                data.len() as f64 / t / 1e6
            );
        }
    }
}
