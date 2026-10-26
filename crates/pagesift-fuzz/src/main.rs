//! stable 工具链下的进程内变异模糊测试（没有 nightly/cargo-fuzz 时用）。
//!
//! ```text
//! pagesift-fuzz <目标> fuzz <种子清单> <秒数> <输出目录> [基础种子]
//! pagesift-fuzz <目标> replay <种子清单> <基础种子> <迭代编号> <输出文件>
//! pagesift-fuzz <目标> run <文件> [迭代编号：决定目标的参数组合]
//! pagesift-fuzz <目标> minimize <文件> <输出文件> [超时毫秒]
//! ```
//! 目标：html、markdown、charset、pdf（见 `targets`）。
//!
//! - panic：输入存成 `<输出目录>/crash-*.bin`，继续跑；
//! - 单个输入超过目标的时间上限：存成 `timeout-*.bin`，进程以 3 退出；
//! - 栈溢出、内存超限这类直接杀掉进程的错误：`<输出目录>/progress-<种子>` 里记着正在跑的
//!   迭代编号，用 `replay` 重新生成那个输入。
//!
//! 内存上限由外面加（`systemd-run --user --scope -p MemoryMax=2G` 或 `ulimit -v`），
//! 见 `scripts/fuzz.sh`。

mod mutate;
mod targets;

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mutate::{Mutator, Rng};
use targets::{TARGETS, Target};

fn usage() -> ExitCode {
    eprintln!("见源码开头的用法说明");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(target) = args
        .first()
        .and_then(|n| TARGETS.iter().find(|t| t.name == n))
    else {
        return usage();
    };
    let rest: Vec<&str> = args[2.min(args.len())..]
        .iter()
        .map(|s| s.as_str())
        .collect();
    match (args.get(1).map(|s| s.as_str()), rest.as_slice()) {
        (Some("fuzz"), [list, secs, out, seed @ ..]) => fuzz(
            target,
            &load_corpus(list, target.max_len),
            Duration::from_secs(secs.parse().unwrap()),
            Path::new(out),
            seed.first().map_or(1, |s| s.parse().unwrap()),
        ),
        (Some("replay"), [list, seed, iter, out]) => {
            let corpus = load_corpus(list, target.max_len);
            let data = generate(
                target,
                &corpus,
                seed.parse().unwrap(),
                iter.parse().unwrap(),
            );
            std::fs::write(out, data).unwrap();
            ExitCode::SUCCESS
        }
        (Some("run"), [file, iter @ ..]) => {
            let data = std::fs::read(file).unwrap();
            let iter = iter.first().map_or(0, |i| i.parse().unwrap());
            let t = Instant::now();
            (target.run)(&data, iter);
            println!(
                "{} 字节，{:.1} ms",
                data.len(),
                t.elapsed().as_secs_f64() * 1e3
            );
            ExitCode::SUCCESS
        }
        (Some("minimize"), [file, out, ms @ ..]) => {
            let limit = ms.first().map_or(target.timeout, |m| {
                Duration::from_millis(m.parse().unwrap())
            });
            minimize(target, Path::new(file), Path::new(out), limit)
        }
        _ => usage(),
    }
}

/// 种子清单：每行一个文件路径。太大的文件截到目标的长度上限。
fn load_corpus(list: &str, max_len: usize) -> Vec<Vec<u8>> {
    let corpus: Vec<Vec<u8>> = std::fs::read_to_string(list)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|p| std::fs::read(p.trim()).ok())
        .map(|mut d| {
            d.truncate(max_len);
            d
        })
        .collect();
    assert!(!corpus.is_empty(), "种子清单是空的");
    corpus
}

fn generate(target: &Target, corpus: &[Vec<u8>], seed: u64, iter: u64) -> Vec<u8> {
    let m = Mutator {
        corpus,
        dict: target.dict,
        max_len: target.max_len,
    };
    m.generate(&mut Rng::for_iteration(seed, iter))
}

struct Shared {
    /// 当前输入开始的时间（相对 `epoch` 的微秒），0 表示空闲。
    started_us: AtomicU64,
    iter: AtomicU64,
    current: Mutex<Arc<Vec<u8>>>,
    stop: AtomicBool,
}

fn fuzz(
    target: &'static Target,
    corpus: &[Vec<u8>],
    budget: Duration,
    out: &Path,
    seed: u64,
) -> ExitCode {
    std::fs::create_dir_all(out).unwrap();
    let corpus: Arc<Vec<Vec<u8>>> = Arc::new(corpus.to_vec());
    let shared = Arc::new(Shared {
        started_us: AtomicU64::new(0),
        iter: AtomicU64::new(0),
        current: Mutex::new(Arc::new(Vec::new())),
        stop: AtomicBool::new(false),
    });
    let epoch = Instant::now();
    let progress = File::create(out.join(format!("progress-{}-{seed}", target.name))).unwrap();
    // panic 信息只打印一行，不带回溯
    std::panic::set_hook(Box::new(|info| {
        eprintln!("panic: {info}");
    }));
    let worker = {
        let shared = shared.clone();
        let corpus = corpus.clone();
        let out: PathBuf = out.to_path_buf();
        // 2 MB 栈，和 tokio 工作线程一样：生产环境里会栈溢出的输入在这里也会
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || worker(target, &corpus, seed, &shared, &progress, &out, epoch))
            .unwrap()
    };
    let deadline = epoch + budget;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let started = shared.started_us.load(Ordering::Acquire);
        if started != 0 {
            let running = epoch
                .elapsed()
                .saturating_sub(Duration::from_micros(started));
            if running > target.timeout {
                let iter = shared.iter.load(Ordering::Acquire);
                let data = shared.current.lock().unwrap().clone();
                let path = out.join(format!("timeout-{}-{seed}-{iter}.bin", target.name));
                std::fs::write(&path, data.as_slice()).unwrap();
                println!(
                    "{{\"target\":\"{}\",\"seed\":{seed},\"result\":\"timeout\",\"iter\":{iter},\"file\":\"{}\"}}",
                    target.name,
                    path.display()
                );
                return ExitCode::from(3);
            }
        }
        // 预算到了只是叫停，最后一个输入照样要在时间上限内跑完
        if Instant::now() >= deadline {
            shared.stop.store(true, Ordering::Release);
        }
        if worker.is_finished() {
            break;
        }
    }
    let stats = worker.join().unwrap();
    println!(
        "{{\"target\":\"{}\",\"seed\":{seed},\"result\":\"ok\",\"secs\":{},\"execs\":{},\"crashes\":{},\"max_ms\":{:.1},\"max_iter\":{},\"max_len\":{}}}",
        target.name,
        budget.as_secs(),
        stats.execs,
        stats.crashes,
        stats.max.as_secs_f64() * 1e3,
        stats.max_iter,
        stats.max_len,
    );
    ExitCode::SUCCESS
}

#[derive(Default)]
struct Stats {
    execs: u64,
    crashes: u64,
    max: Duration,
    max_iter: u64,
    max_len: usize,
}

fn worker(
    target: &Target,
    corpus: &[Vec<u8>],
    seed: u64,
    shared: &Shared,
    progress: &File,
    out: &Path,
    epoch: Instant,
) -> Stats {
    let mut stats = Stats::default();
    let mut iter = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        let data = Arc::new(generate(target, corpus, seed, iter));
        *shared.current.lock().unwrap() = data.clone();
        shared.iter.store(iter, Ordering::Release);
        // 进程被杀时靠它找回输入
        let _ = progress.write_at(&iter.to_le_bytes(), 0);
        let t = Instant::now();
        shared
            .started_us
            .store((t - epoch).as_micros().max(1) as u64, Ordering::Release);
        let r = std::panic::catch_unwind(|| (target.run)(&data, iter));
        let took = t.elapsed();
        shared.started_us.store(0, Ordering::Release);
        if r.is_err() {
            stats.crashes += 1;
            let path = out.join(format!("crash-{}-{seed}-{iter}.bin", target.name));
            std::fs::write(&path, data.as_slice()).unwrap();
            eprintln!("崩溃：{}", path.display());
        }
        if took > stats.max {
            stats.max = took;
            stats.max_iter = iter;
            stats.max_len = data.len();
        }
        stats.execs += 1;
        iter += 1;
    }
    stats
}

/// 用子进程判断输入还会不会出问题（panic、被杀、或者超过 `limit`），
/// 按块删除（ddmin 的简化版）把输入缩到最小。
fn minimize(target: &Target, file: &Path, out: &Path, limit: Duration) -> ExitCode {
    let exe = std::env::current_exe().unwrap();
    let tmp = out.with_extension("try");
    let fails = |data: &[u8]| -> bool {
        std::fs::write(&tmp, data).unwrap();
        let t = Instant::now();
        let mut child = Command::new(&exe)
            .args([target.name, "run"])
            .arg(&tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return !status.success();
            }
            if t.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let mut data = std::fs::read(file).unwrap();
    if !fails(&data) {
        eprintln!("原输入没有复现问题");
        return ExitCode::from(1);
    }
    let mut chunk = data.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        let mut changed = false;
        while i < data.len() {
            let end = (i + chunk).min(data.len());
            let mut cand = data[..i].to_vec();
            cand.extend_from_slice(&data[end..]);
            if !cand.is_empty() && fails(&cand) {
                data = cand;
                changed = true;
            } else {
                i += chunk;
            }
        }
        if !changed {
            chunk /= 2;
        }
    }
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(out, &data).unwrap();
    println!("最小化到 {} 字节：{}", data.len(), out.display());
    ExitCode::SUCCESS
}
