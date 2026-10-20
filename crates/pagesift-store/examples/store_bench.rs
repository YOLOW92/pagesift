//! 测索引内存、空闲内存和启动重建耗时。
//!
//! ```text
//! cargo run --release -p pagesift-store --example store_bench -- [条数...] [--dir 目录]
//! ```
//!
//! 默认测 1 万、10 万、100 万条。每项在单独的子进程里测，RSS 互不干扰。
//! 数据默认写到 `target/store-bench`（别放在 tmpfs 上，否则文件本身也占内存）。
//! RSS 读 /proc/self/status，只在 Linux 上有数。

use std::collections::HashMap;
use std::env;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use pagesift_store::{Config, Store, SyncPolicy};

const VALUE_LEN: usize = 300;

fn key(i: u64) -> String {
    format!(
        "https://www.example{}.com/articles/{}/page?id={i}&lang=en",
        i % 1000,
        i / 1000
    )
}

fn config(dir: &Path) -> Config {
    let mut c = Config::new(dir);
    c.sync = SyncPolicy::Never;
    c.background = false;
    c
}

/// (VmRSS, VmHWM)，单位 KiB。
fn rss() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        s.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn child(args: &[String]) {
    match args[0].as_str() {
        "empty" => {
            let before = rss().0;
            let store = Store::open(config(Path::new(&args[1]))).unwrap();
            let after = rss().0;
            black_box(&store);
            println!("{before} {after}");
        }
        "populate" => {
            let n: u64 = args[2].parse().unwrap();
            let store = Store::open(config(Path::new(&args[1]))).unwrap();
            let value = vec![b'v'; VALUE_LEN];
            let t = Instant::now();
            for i in 0..n {
                store.cache().put(key(i), &value).unwrap();
            }
            store.flush().unwrap();
            let secs = t.elapsed().as_secs_f64();
            let st = store.stats();
            println!("{} {} {}", n as f64 / secs, st.disk_bytes, rss().1);
        }
        "reopen" => {
            let n: u64 = args[2].parse().unwrap();
            let before = rss().0;
            let t = Instant::now();
            let store = Store::open(config(Path::new(&args[1]))).unwrap();
            let open_ms = t.elapsed().as_secs_f64() * 1000.0;
            let (after, peak) = rss();
            let st = store.stats();
            assert_eq!(st.entries, n);
            let lookups = n.min(200_000);
            let t = Instant::now();
            let mut x = 0u64;
            for i in 0..lookups {
                let k = key((i * 7919) % n);
                x += store.cache().entry(&k).unwrap().unwrap().value.len() as u64;
            }
            let get_ns = t.elapsed().as_nanos() as f64 / lookups as f64;
            black_box(x);
            println!(
                "{open_ms} {} {before} {after} {peak} {get_ns}",
                st.index_bytes
            );
        }
        "naive" => {
            // 对照：键整串放在内存里的索引（HashMap<Box<[u8]>, 位置>）。
            let n: u64 = args[1].parse().unwrap();
            let before = rss().0;
            let mut m: HashMap<Box<[u8]>, [u32; 4]> = HashMap::new();
            for i in 0..n {
                m.insert(key(i).into_bytes().into_boxed_slice(), [i as u32; 4]);
            }
            let after = rss().0;
            black_box(&m);
            println!("{before} {after}");
        }
        other => panic!("unknown mode {other}"),
    }
}

fn run(exe: &Path, args: &[&str]) -> Vec<f64> {
    let out = Command::new(exe)
        .arg("--child")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .map(|v| v.parse().unwrap())
        .collect()
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--child") {
        child(&args[1..]);
        return;
    }
    let mut sizes = Vec::new();
    let mut root = PathBuf::from("target/store-bench");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--dir" {
            root = PathBuf::from(it.next().expect("--dir needs a value"));
        } else {
            sizes.push(a.parse::<u64>().expect("record count"));
        }
    }
    if sizes.is_empty() {
        sizes = vec![10_000, 100_000, 1_000_000];
    }
    let exe = env::current_exe().unwrap();
    let _ = std::fs::remove_dir_all(&root);

    let empty = root.join("empty");
    let e = run(&exe, &["empty", empty.to_str().unwrap()]);
    println!("## 空存储（越低越好）\n");
    println!(
        "进程 RSS：打开前 {:.1} MiB，打开空存储后 {:.1} MiB（存储本身 +{:.2} MiB）\n",
        mib(e[0] as u64),
        mib(e[1] as u64),
        mib((e[1] - e[0]) as u64)
    );

    println!(
        "| 条数 | 索引估算 MiB（越低越好） | 重建后 RSS 增量 MiB（越低越好） | 整串键索引 RSS 增量 MiB（对照，越低越好） | 启动重建 ms（越低越好） | 重建峰值 RSS MiB（越低越好） | 写入 条/秒（越高越好） | 随机读 µs/条（越低越好） | 磁盘 MiB |"
    );
    println!("|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for &n in &sizes {
        let dir = root.join(n.to_string());
        let d = dir.to_str().unwrap();
        let p = run(&exe, &["populate", d, &n.to_string()]);
        let r = run(&exe, &["reopen", d, &n.to_string()]);
        let naive = run(&exe, &["naive", &n.to_string()]);
        println!(
            "| {n} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.0} | {:.2} | {:.0} |",
            r[1] / 1048576.0,
            mib((r[3] - r[2]) as u64),
            mib((naive[1] - naive[0]) as u64),
            r[0],
            mib(r[4] as u64),
            p[0],
            r[5] / 1000.0,
            p[1] / 1048576.0,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = std::fs::remove_dir_all(&root);
}
