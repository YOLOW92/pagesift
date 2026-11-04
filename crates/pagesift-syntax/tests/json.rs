//! JSON 语法的端到端测试：官方 corpus 全部通过，加上畸形输入和随机变异不崩溃。

use pagesift_syntax::Limits;
use pagesift_syntax::api::parse;
use pagesift_syntax::corpus::{Report, parse_corpus, run_cases};
use pagesift_syntax::table::Language;

fn lang() -> Language {
    Language::from_bytes(include_bytes!("data/json.pslg")).expect("表有效")
}

#[test]
fn table_roundtrip() {
    let l = lang();
    assert_eq!(Language::from_bytes(&l.to_bytes()).unwrap(), l);
}

#[test]
fn official_corpus_passes() {
    let l = lang();
    let cases = parse_corpus(include_str!("data/json-corpus.txt"));
    assert!(cases.len() >= 7);
    let mut rep = Report::default();
    run_cases(&l, "main.txt", &cases, &mut rep);
    assert_eq!(rep.passed, rep.total, "失败用例: {:?}", rep.failures);
}

#[test]
fn malformed_inputs_return_trees() {
    let l = lang();
    for s in [
        "",
        "[",
        "{\"a\":",
        "]]]]",
        "{\"a\" 1}",
        "\u{0}",
        "[1,,2]",
        "\"abc",
        "[[[[[[[[[[",
    ] {
        let t = parse(&l, s.as_bytes(), Limits::default());
        assert!(t.has_error() || s.is_empty());
        let _ = t.to_sexp(&l);
    }
    // 非法 UTF-8
    let t = parse(&l, &[b'[', 0xff, 0xfe, b']'], Limits::default());
    assert!(t.has_error());
}

#[test]
fn limits_bound_deep_nesting() {
    let l = lang();
    let deep = "[".repeat(200_000);
    let lim = Limits {
        max_stack_depth: 1000,
        ..Limits::default()
    };
    let t = parse(&l, deep.as_bytes(), lim);
    assert!(t.truncated);
}

/// 随机变异：固定种子的简单 xorshift，保证可复现。
#[test]
fn random_mutation_never_panics() {
    let l = lang();
    let base = br#"{"a": [1, 2.5e3, true, null, "x\n"], "b": {"c": "d"}} // c"#.to_vec();
    let mut x = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..3000 {
        let mut b = base.clone();
        for _ in 0..(next() % 6 + 1) {
            let i = (next() as usize) % b.len().max(1);
            match next() % 4 {
                0 if !b.is_empty() => b[i] = next() as u8,
                1 if !b.is_empty() => {
                    b.remove(i);
                }
                2 => b.insert(i.min(b.len()), b"[]{},:\"\\ /"[(next() % 10) as usize]),
                _ => {
                    let j = (next() as usize) % b.len().max(1);
                    let c = b[i.min(b.len().saturating_sub(1))..].len().min(5);
                    let seg: Vec<u8> = b[i.min(b.len())..(i + c).min(b.len())].to_vec();
                    b.splice(j.min(b.len())..j.min(b.len()), seg);
                }
            }
        }
        let t = parse(&l, &b, Limits::default());
        let s = t.to_sexp(&l);
        assert!(!s.is_empty());
    }
}
