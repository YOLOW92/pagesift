//! 用法：ts-corpus <语法表.pslg> <corpus 目录或文件> [-v] [--only 名字片段]
use pagesift_syntax::corpus::{Report, parse_corpus, run_cases};
use pagesift_syntax::table::Language;

fn collect(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        v.sort();
        for x in v {
            collect(&x, out);
        }
    } else if p.extension().is_some_and(|e| e == "txt") {
        out.push(p.to_path_buf());
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("用法: ts-corpus <表> <corpus> [-v] [--only 片段]");
        std::process::exit(2);
    }
    let lang = Language::from_bytes(&std::fs::read(&a[1]).expect("读表失败")).expect("表无效");
    let verbose = a.iter().any(|x| x == "-v");
    let only = a
        .iter()
        .position(|x| x == "--only")
        .and_then(|i| a.get(i + 1))
        .cloned();
    let mut files = Vec::new();
    collect(std::path::Path::new(&a[2]), &mut files);
    let mut rep = Report::default();
    for f in files {
        let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let mut cases = parse_corpus(&text);
        if let Some(o) = &only {
            cases.retain(|c| c.name.contains(o.as_str()));
        }
        let before = (rep.total, rep.passed);
        run_cases(&lang, &name, &cases, &mut rep);
        println!("{name}: {}/{}", rep.passed - before.1, rep.total - before.0);
    }
    println!(
        "{}: 通过 {}/{}（按 :skip/:cst 跳过 {}）",
        lang.name, rep.passed, rep.total, rep.skipped
    );
    if verbose {
        for (n, e, ac) in &rep.failures {
            println!("--- 失败: {n}\n期望: {e}\n实际: {ac}");
        }
    } else {
        for (n, _, _) in &rep.failures {
            println!("失败: {n}");
        }
    }
}
