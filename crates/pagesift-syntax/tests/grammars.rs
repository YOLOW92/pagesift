//! C / Go / Java 的 corpus 与随机变异测试。表和语法仓库在仓库之外：
//! 设置 `PSLG_DIR`（放 c.pslg、go.pslg、java.pslg）和 `TS_SRC`（放各语法仓库的克隆）才会运行，否则直接跳过。

use pagesift_syntax::Limits;
use pagesift_syntax::api::parse;
use pagesift_syntax::corpus::{Report, parse_corpus, run_cases};
use pagesift_syntax::table::Language;
use std::path::PathBuf;

fn dirs() -> Option<(PathBuf, PathBuf)> {
    Some((
        std::env::var_os("PSLG_DIR")?.into(),
        std::env::var_os("TS_SRC")?.into(),
    ))
}

fn corpus_files(d: &std::path::Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    v.sort();
    v
}

#[test]
fn corpus_all_pass() {
    let Some((t, s)) = dirs() else { return };
    for name in ["c", "go", "java"] {
        let l =
            Language::from_bytes(&std::fs::read(t.join(format!("{name}.pslg"))).unwrap()).unwrap();
        let mut rep = Report::default();
        for f in corpus_files(&s.join(format!("tree-sitter-{name}/test/corpus"))) {
            let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
            run_cases(&l, &f.display().to_string(), &parse_corpus(&text), &mut rep);
        }
        assert_eq!(
            rep.passed,
            rep.total,
            "{name} 失败: {:?}",
            rep.failures.iter().map(|f| &f.0).collect::<Vec<_>>()
        );
    }
}

#[test]
fn mutation_never_panics() {
    let Some((t, s)) = dirs() else { return };
    let mut x = 0x2545F4914F6CDD1Du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for name in ["c", "go", "java"] {
        let l =
            Language::from_bytes(&std::fs::read(t.join(format!("{name}.pslg"))).unwrap()).unwrap();
        let mut seeds: Vec<Vec<u8>> = Vec::new();
        for f in corpus_files(&s.join(format!("tree-sitter-{name}/test/corpus"))) {
            let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
            seeds.extend(parse_corpus(&text).into_iter().map(|c| c.input));
        }
        for i in 0..1500 {
            let mut b = seeds[(next() as usize) % seeds.len()].clone();
            for _ in 0..(next() % 5 + 1) {
                if b.is_empty() {
                    break;
                }
                let p = (next() as usize) % b.len();
                match next() % 3 {
                    0 => b[p] = next() as u8,
                    1 => {
                        b.remove(p);
                    }
                    _ => b.insert(p, b"{}()[];,\"'/*#<>\\ \n"[(next() % 18) as usize]),
                }
            }
            let lim = Limits {
                max_nodes: 200_000,
                ..Limits::default()
            };
            let tr = parse(&l, &b, lim);
            assert!(!tr.to_sexp(&l).is_empty(), "{name} 第 {i} 个变异");
        }
    }
}
