//! 变异压力测试：对清单里的每个 PDF 做若干次随机截断 / 翻转字节 / 删除片段，
//! 直接调用不经 `catch_unwind` 的入口，让 panic 暴露出来；打印每次变异的耗时，便于发现卡死。
//! `pdf_mutate <清单> <每个文件的变异次数> [种子]`
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let list = args.next().expect("usage: pdf_mutate <list> <n> [seed]");
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let mut seed: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let opts = pagesift_pdf::Options {
        timeout: Some(std::time::Duration::from_secs(10)),
        ..Default::default()
    };
    for path in std::fs::read_to_string(list).expect("list").lines() {
        let orig = std::fs::read(path).expect("read");
        if orig.len() < 16 {
            continue;
        }
        for k in 0..n {
            let mut b = orig.clone();
            let len = b.len();
            match rand() % 3 {
                0 => b.truncate(16 + (rand() as usize) % (len - 16)),
                1 => {
                    for _ in 0..1 + rand() % 16 {
                        let i = (rand() as usize) % len;
                        b[i] ^= 1 << (rand() % 8);
                    }
                }
                _ => {
                    let i = (rand() as usize) % len;
                    let j = (i + 1 + (rand() as usize) % 4096).min(len);
                    b.drain(i..j);
                }
            }
            println!("BEGIN {path} #{k}");
            let t = Instant::now();
            let r = pagesift_pdf::fuzzing::extract_unguarded(&b, &opts);
            println!(
                "END {path} #{k} {:.1} ms {}",
                t.elapsed().as_secs_f64() * 1000.0,
                r.is_ok()
            );
        }
    }
}
