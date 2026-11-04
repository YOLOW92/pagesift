//! 用法：ts-parse <语法表> <源文件> [-s 打印S表达式] [-n 重复次数]
use pagesift_syntax::{Limits, api::parse, table::Language};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("用法: ts-parse <表> <文件> [-s] [-n 次数]");
        std::process::exit(2);
    }
    let lang = Language::from_bytes(&std::fs::read(&a[1]).expect("读表失败")).expect("表无效");
    let src = std::fs::read(&a[2]).expect("读源文件失败");
    let n: usize = a
        .iter()
        .position(|x| x == "-n")
        .and_then(|i| a.get(i + 1))
        .and_then(|x| x.parse().ok())
        .unwrap_or(1);
    let t0 = std::time::Instant::now();
    let mut tree = None;
    for _ in 0..n {
        tree = Some(parse(&lang, &src, Limits::default()));
    }
    let el = t0.elapsed().as_secs_f64() / n as f64;
    let tree = tree.unwrap();
    if a.iter().any(|x| x == "-s") {
        println!("{}", tree.to_sexp(&lang));
    }
    if a.iter().any(|x| x == "-e") {
        let mut shown = 0;
        tree.visit(&lang, |ev| {
            if let pagesift_syntax::api::Ev::Open(n) = ev
                && (n.error || n.missing)
                && shown < 8
            {
                shown += 1;
                let line = src[..n.start.min(src.len())]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count()
                    + 1;
                let end = (n.end.min(src.len())).min(n.start + 60);
                let text =
                    String::from_utf8_lossy(&src[n.start.min(end)..end]).replace('\n', "\\n");
                eprintln!(
                    "错误节点 {} 第 {} 行 [{}..{}]: {}",
                    n.kind, line, n.start, n.end, text
                );
            }
        });
    }
    eprintln!(
        "{} 字节 {:.2} ms ({:.1} MB/s) 错误={} 截断={} 节点={}",
        src.len(),
        el * 1e3,
        src.len() as f64 / el / 1e6,
        tree.has_error(),
        tree.truncated,
        tree.arena.nodes.len()
    );
}
