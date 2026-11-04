//! 往返、真实样本（环境变量 PAGESIFT_ZSTD_SAMPLES 指向的目录里的 *.zst 与同名原文）、随机变异。
use pagesift_zstd::{compress, decompress};

fn lcg(s: &mut u64) -> u64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *s >> 33
}

fn sample(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    let words: Vec<String> = (0..50).map(|i| format!("<div class=\"w{i}\">")).collect();
    let mut v = Vec::new();
    while v.len() < n {
        let r = lcg(&mut s);
        if r % 5 == 0 {
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
    for (i, n) in [0usize, 1, 3, 4, 100, 5000, 131072, 131073, 400_000].into_iter().enumerate() {
        let d = sample(n, i as u64 + 1);
        for level in [0, 3] {
            let z = compress(&d, level);
            assert_eq!(decompress(&z, usize::MAX).unwrap(), d, "n={n} level={level}");
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
        if lcg(&mut s) % 4 == 0 {
            m.truncate(lcg(&mut s) as usize % m.len());
        }
        let _ = decompress(&m, 1 << 20);
    }
}

#[test]
fn real_samples() {
    let Ok(dir) = std::env::var("PAGESIFT_ZSTD_SAMPLES") else { return };
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
    let (Ok(src), Ok(dst)) = (std::env::var("PAGESIFT_ZSTD_SAMPLES"), std::env::var("PAGESIFT_ZSTD_DUMP")) else { return };
    for name in ["a.txt", "b.txt", "e.html", "d.bin", "g.bin", "f.bin"] {
        let d = std::fs::read(format!("{src}/{name}")).unwrap();
        std::fs::write(format!("{dst}/{name}.zst"), compress(&d, 3)).unwrap();
    }
}
