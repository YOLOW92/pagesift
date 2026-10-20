//! 仅 release 下断言耗时：`cargo test --release -p pagesift-diff`。
#![cfg(not(debug_assertions))]

use std::time::Instant;

use pagesift_diff::diff;

fn doc(n: usize, tweak: bool) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < (1 << 20) {
        let extra = if tweak && i % 40 == 3 { " edited" } else { "" };
        s.push_str(&format!(
            "Paragraph {i}: the quick brown fox jumps over the lazy dog {}{extra}.\n\n",
            i * 7919 % n
        ));
        i += 1;
    }
    s
}

#[test]
fn one_mb_under_50ms() {
    let (a, b) = (doc(1000, false), doc(1000, true));
    let t = Instant::now();
    let r = diff(&a, &b);
    let el = t.elapsed();
    assert!(r.stats.modified > 100);
    assert!(el.as_millis() < 50, "took {el:?}");
}
