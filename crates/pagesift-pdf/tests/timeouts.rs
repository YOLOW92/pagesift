//! `Options::timeout` 管住每个阶段：文件修复扫描、结构树、内容流之后的表格识别和图区识别。
//! 每个用例都是“正常规模的处理逻辑 + 极端规模的输入”，设 300 ms 超时：要么正常返回，要么返回 Timeout，
//! 都必须在 1.5 s 内结束（留出机器忙时的余量）。

mod common;

use std::fmt::Write;
use std::time::{Duration, Instant};

use common::{Builder, Page, line};
use pagesift_pdf::{Error, Options, fuzzing};

const TIMEOUT: Duration = Duration::from_millis(300);
const LIMIT: Duration = Duration::from_millis(1500);

fn timed(data: &[u8]) -> (Duration, Result<(), Error>) {
    let opts = Options {
        timeout: Some(TIMEOUT),
        max_pages: 100_000,
        ..Options::default()
    };
    let t = Instant::now();
    let r = fuzzing::extract_unguarded(data, &opts).map(|_| ());
    (t.elapsed(), r)
}

fn assert_bounded(name: &str, data: &[u8]) {
    let (took, r) = timed(data);
    assert!(took < LIMIT, "{name}: {took:?}（{r:?}）超过 {LIMIT:?}");
}

/// 大量互不相连的小矩形和斜线，中间夹着短标签：图区识别的证据和聚类。
#[test]
fn many_shapes_on_one_page() {
    let mut c = String::new();
    for i in 0..60_000 {
        let x = (i % 300) as f64 * 2.0;
        let y = (i / 300) as f64 * 3.5;
        let _ = write!(
            c,
            "{x} {y} 1 {} re f {x} {y} m {} {} l S ",
            1 + i % 7,
            x + 1.5,
            y + 2.0
        );
    }
    for i in 0..400 {
        c.push_str(&line(
            "F1",
            6.0,
            (i % 20) as f64 * 30.0,
            (i / 20) as f64 * 35.0,
            "12.5",
        ));
    }
    assert_bounded("many_shapes", &Builder::new(vec![Page::text(c)]).build());
}

/// 横竖各 400 条框线织成的网格，每格一个数字：表格识别的格子和单元格分配。
#[test]
fn dense_ruled_grid() {
    let mut c = String::new();
    for i in 0..400 {
        let p = i as f64 * 1.9;
        let _ = write!(c, "0 {p} m 760 {p} l S {p} 0 m {p} 760 l S ");
    }
    for i in 0..3000 {
        c.push_str(&line(
            "F1",
            1.0,
            (i % 60) as f64 * 12.6 + 0.3,
            (i / 60) as f64 * 15.0 + 0.3,
            "7",
        ));
    }
    assert_bounded("dense_grid", &Builder::new(vec![Page::text(c)]).build());
}

/// 没有 xref、64 MB 的对象堆：全文修复扫描。
#[test]
fn huge_file_needing_repair() {
    let mut data = b"%PDF-1.7\n".to_vec();
    let mut n = 1u32;
    while data.len() < 64 << 20 {
        data.extend_from_slice(format!("{n} 0 obj\n<< /K [{} 0 R] >>\nendobj\n", n + 1).as_bytes());
        n += 1;
    }
    assert_bounded("huge_repair", &data);
}

/// 二十万个元素的结构树（每个元素挂三个 MCID）。
#[test]
fn huge_structure_tree() {
    use lopdf::{Object, dictionary};
    let mut doc = Builder::new(vec![Page::text(line("F1", 11.0, 72.0, 700.0, "Hi"))]).document();
    let page = *doc.get_pages().values().next().unwrap();
    let mut kids = Vec::new();
    for i in 0..200_000i64 {
        let id = doc.add_object(dictionary! {
            "S" => "H1", "Pg" => page,
            "K" => vec![Object::Integer(i), Object::Integer(i + 1), Object::Integer(i + 2)],
        });
        kids.push(Object::Reference(id));
    }
    let tree = doc.add_object(dictionary! { "Type" => "StructTreeRoot", "K" => kids });
    let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    doc.get_dictionary_mut(root)
        .unwrap()
        .set("StructTreeRoot", tree);
    assert_bounded("huge_struct_tree", &common::save(doc));
}
