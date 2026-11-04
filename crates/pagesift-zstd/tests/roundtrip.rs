//! 往返、真实样本（环境变量 PAGESIFT_ZSTD_SAMPLES 指向的目录里的 *.zst 与同名原文）、随机变异。
use pagesift_zstd::{compress, decompress};

fn lcg(s: &mut u64) -> u64 {
    *s = s
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *s >> 33
}

fn sample(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    let words: Vec<String> = (0..50).map(|i| format!("<div class=\"w{i}\">")).collect();
    let mut v = Vec::new();
    while v.len() < n {
        let r = lcg(&mut s);
        if r.is_multiple_of(5) {
            v.push((r >> 8) as u8);
        } else {
            v.extend_from_slice(words[(r >> 4) as usize % 50].as_bytes());
        }
    }
    v.truncate(n);
    v
}

#[test]
fn roundtrip_sizes() {
    for (i, n) in [0usize, 1, 3, 4, 100, 5000, 131072, 131073, 400_000]
        .into_iter()
        .enumerate()
    {
        let d = sample(n, i as u64 + 1);
        for level in [0, 3] {
            let z = compress(&d, level);
            assert_eq!(
                decompress(&z, usize::MAX).unwrap(),
                d,
                "n={n} level={level}"
            );
        }
    }
}

#[test]
fn output_limit() {
    let d = sample(10_000, 9);
    let z = compress(&d, 3);
    assert!(decompress(&z, 100).is_err());
    assert!(decompress(&z, 10_000).is_ok());
}

#[test]
fn mutation_never_panics() {
    let d = sample(20_000, 7);
    let z = compress(&d, 3);
    let mut s = 42u64;
    for _ in 0..3000 {
        let mut m = z.clone();
        for _ in 0..1 + lcg(&mut s) % 4 {
            let i = lcg(&mut s) as usize % m.len();
            m[i] = lcg(&mut s) as u8;
        }
        if lcg(&mut s).is_multiple_of(4) {
            m.truncate(lcg(&mut s) as usize % m.len());
        }
        let _ = decompress(&m, 1 << 20);
    }
}

#[test]
fn real_samples() {
    let Ok(dir) = std::env::var("PAGESIFT_ZSTD_SAMPLES") else {
        return;
    };
    let mut n = 0;
    let mut s = 1u64;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "zst") {
            let z = std::fs::read(&p).unwrap();
            let want = std::fs::read(p.with_extension("")).unwrap();
            assert_eq!(decompress(&z, usize::MAX).unwrap(), want, "{p:?}");
            // 对真实样本也做变异
            for _ in 0..300 {
                let mut m = z.clone();
                let i = lcg(&mut s) as usize % m.len();
                m[i] ^= 1 << (lcg(&mut s) % 8);
                let _ = decompress(&m, 64 << 20);
            }
            n += 1;
        }
    }
    eprintln!("real samples ok: {n}");
}

/// 把本编码器的输出写到 PAGESIFT_ZSTD_DUMP 目录，供官方 `zstd -d` 验证。
#[test]
fn dump_for_official_decoder() {
    let (Ok(src), Ok(dst)) = (
        std::env::var("PAGESIFT_ZSTD_SAMPLES"),
        std::env::var("PAGESIFT_ZSTD_DUMP"),
    ) else {
        return;
    };
    for name in ["a.txt", "b.txt", "e.html", "d.bin", "g.bin", "f.bin"] {
        let d = std::fs::read(format!("{src}/{name}")).unwrap();
        std::fs::write(format!("{dst}/{name}.zst"), compress(&d, 3)).unwrap();
    }
}

/// 解码速度：PAGESIFT_ZSTD_SAMPLES 目录里的 b.txt.l3.zst，与官方 `zstd -d` 命令交替各跑 15 次。
#[test]
fn bench_vs_cli() {
    let Ok(src) = std::env::var("PAGESIFT_ZSTD_SAMPLES") else {
        return;
    };
    use std::time::Instant;
    for name in ["b.txt.l3.zst", "e.html.l3.zst", "b.txt.l19.zst"] {
        let path = format!("{src}/{name}");
        let z = std::fs::read(&path).unwrap();
        let (mut ours, mut cli) = (f64::MAX, f64::MAX);
        let mut size = 0;
        for _ in 0..15 {
            let t = Instant::now();
            size = decompress(&z, usize::MAX).unwrap().len();
            ours = ours.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            let o = std::process::Command::new("zstd")
                .args(["-dcq", &path])
                .output()
                .unwrap();
            assert_eq!(o.stdout.len(), size);
            cli = cli.min(t.elapsed().as_secs_f64());
        }
        let mb = size as f64 / 1e6;
        eprintln!(
            "{name}: ours {:.0} MB/s, C cli(含进程启动与管道) {:.0} MB/s",
            mb / ours,
            mb / cli
        );
    }
}

/// 字节分布偏斜、覆盖全部 256 种取值(触发 FSE 压缩权重表和 4 流 Huffman)。
fn skewed(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            let r = lcg(&mut s);
            let k = (r % 4096) as f64;
            ((k * k / 4096.0 / 16.0) as u32 % 256) as u8
        })
        .collect()
}

#[test]
fn huffman_literals_roundtrip() {
    for n in [40usize, 300, 5000, 100_000, 300_000] {
        let d = skewed(n, n as u64);
        let z = compress(&d, 3);
        assert_eq!(decompress(&z, usize::MAX).unwrap(), d, "n={n}");
        if n >= 5000 {
            assert!(z.len() < d.len(), "n={n} {}", z.len());
        }
    }
}

#[test]
fn dump_skewed() {
    if let Ok(dst) = std::env::var("PAGESIFT_ZSTD_DUMP") {
        std::fs::write(
            format!("{dst}/skewed.zst"),
            compress(&skewed(300_000, 5), 3),
        )
        .unwrap();
        std::fs::write(format!("{dst}/skewed.raw"), skewed(300_000, 5)).unwrap();
    }
}

/// 编码速度:与官方 `zstd -3` 命令交替各跑 5 次(取最快)。
#[test]
fn bench_encode_vs_cli() {
    let Ok(src) = std::env::var("PAGESIFT_ZSTD_SAMPLES") else {
        return;
    };
    use std::time::Instant;
    for name in ["b.txt", "e.html"] {
        let path = format!("{src}/{name}");
        let d = std::fs::read(&path).unwrap();
        let (mut ours, mut cli) = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            let t = Instant::now();
            let z = compress(&d, 3);
            ours = ours.min(t.elapsed().as_secs_f64());
            assert!(!z.is_empty());
            let t = Instant::now();
            let o = std::process::Command::new("zstd")
                .args(["-3cq", &path])
                .output()
                .unwrap();
            assert!(!o.stdout.is_empty());
            cli = cli.min(t.elapsed().as_secs_f64());
        }
        let mb = d.len() as f64 / 1e6;
        eprintln!(
            "encode {name}: ours {:.0} MB/s, C cli {:.0} MB/s",
            mb / ours,
            mb / cli
        );
    }
}

#[test]
fn checksum_is_verified() {
    let d = sample(50_000, 3);
    let mut z = compress(&d, 3);
    assert_eq!(decompress(&z, usize::MAX).unwrap(), d);
    let n = z.len();
    z[n - 1] ^= 1;
    assert!(decompress(&z, usize::MAX).is_err());
}
