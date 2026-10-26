//! html5lib-tests 树构建用例（现在由 web-platform-tests 维护）逐条对照。
//!
//! 数据不进仓库，按 tasks/0.80/b1-b3/RESULT.md 的说明稀疏检出到
//! `bench/data/wpt-parsing`；也可以用环境变量 `PAGESIFT_HTML5LIB_DIR` 指向放 `.dat` 的目录。
//! 数据缺失时跳过。
//!
//! 不跑的用例：`#document-fragment`（pagesift 没有片段解析接口）和 `#script-off`
//! （pagesift 按开启脚本的规则解析：`<noscript>` 的内容是原始文本）。
//!
//! 每条用例分成 通过 / 影响正文 / 只差行内结构 / 其他 四类（见 `classify`）。影响正文的差异
//! 必须写在 `tests/html5lib_known.txt` 里并说明原因，否则测试失败。
//! `PAGESIFT_HTML5LIB_REPORT=路径` 时把每条没通过的用例（输入、预期、实际）写进报告。

mod classify;
mod dat;
mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use classify::Class;
use pagesift_html::Html;

fn data_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("PAGESIFT_HTML5LIB_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../bench/data/wpt-parsing/html/syntax/parsing/resources"),
    };
    dir.is_dir().then_some(dir)
}

fn known() -> BTreeMap<String, String> {
    let text = include_str!("../html5lib_known.txt");
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (id, why) = l.split_once('\t')?;
            Some((id.trim().to_string(), why.trim().to_string()))
        })
        .collect()
}

#[test]
fn html5lib_tree_construction() {
    let Some(dir) = data_dir() else {
        eprintln!("html5lib-tests 数据不在，跳过（见文件开头的说明）");
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "dat"))
        .collect();
    files.sort();
    let known = known();
    let mut counts: BTreeMap<Class, usize> = BTreeMap::new();
    let mut skipped_fragment = 0;
    let mut skipped_script_off = 0;
    let mut panics = Vec::new();
    let mut slow = Vec::new();
    let mut unexplained = Vec::new();
    let mut seen_content = BTreeSet::new();
    let mut report = String::new();
    let mut per_file: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for path in &files {
        let fname = path.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(path).unwrap();
        for case in dat::parse(&text) {
            let id = format!("{fname}#{}", case.index);
            if case.fragment.is_some() {
                skipped_fragment += 1;
                continue;
            }
            if case.scripting == Some(false) {
                skipped_script_off += 1;
                continue;
            }
            let expected = tree::parse_expected(&case.document);
            let start = Instant::now();
            let actual = std::panic::catch_unwind(|| tree::from_html(&Html::parse(&case.data)));
            let elapsed = start.elapsed();
            if elapsed > Duration::from_millis(500) {
                slow.push(format!("{id} {elapsed:?}"));
            }
            let Ok(actual) = actual else {
                panics.push(id);
                continue;
            };
            let class = classify::classify(&expected, &actual);
            *counts.entry(class).or_default() += 1;
            let e = per_file.entry(fname.clone()).or_default();
            e.1 += 1;
            if class == Class::Pass {
                e.0 += 1;
                continue;
            }
            if class == Class::Content {
                seen_content.insert(id.clone());
                if !known.contains_key(&id) {
                    unexplained.push(id.clone());
                }
            }
            let _ = write!(
                report,
                "## {id} [{}]{}\n### data\n{}\n### expected\n{}### actual\n{}\n",
                class.label(),
                known
                    .get(&id)
                    .map(|w| format!(" 已知：{w}"))
                    .unwrap_or_default(),
                case.data,
                tree::dump(&expected),
                tree::dump(&actual),
            );
        }
    }
    let total: usize = counts.values().sum();
    let pass = counts.get(&Class::Pass).copied().unwrap_or(0);
    let mut summary = format!(
        "html5lib 树构建：跑了 {total} 条（跳过片段 {skipped_fragment}、script-off {skipped_script_off}），\
         通过 {pass}（{:.1}%）",
        100.0 * pass as f64 / total.max(1) as f64
    );
    for (c, n) in &counts {
        if *c != Class::Pass {
            let _ = write!(summary, "，{} {n}", c.label());
        }
    }
    let _ = write!(
        summary,
        "，崩溃 {}，超过 500 ms {}",
        panics.len(),
        slow.len()
    );
    println!("{summary}");
    for (f, (p, t)) in &per_file {
        println!("  {f}: {p}/{t}");
    }
    if let Some(out) = std::env::var_os("PAGESIFT_HTML5LIB_REPORT") {
        std::fs::write(out, format!("# {summary}\n\n{report}")).unwrap();
    }
    let stale: Vec<&String> = known
        .keys()
        .filter(|k| !seen_content.contains(*k))
        .collect();
    if !stale.is_empty() {
        println!("已知清单里这些用例已经不再影响正文，可以删掉：{stale:?}");
    }
    assert!(panics.is_empty(), "解析时崩溃：{panics:?}");
    assert!(slow.is_empty(), "解析太慢：{slow:?}");
    assert!(
        unexplained.is_empty(),
        "{} 条影响正文的差异没有写进 tests/html5lib_known.txt，前 20 条：{:?}",
        unexplained.len(),
        &unexplained[..unexplained.len().min(20)]
    );
}
