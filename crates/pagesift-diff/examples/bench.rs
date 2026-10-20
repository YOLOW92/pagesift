//! cargo run --release -p pagesift-diff --example bench
use std::time::Instant;

use pagesift_diff::diff;

fn make_doc(seed: u64, n: usize) -> String {
    let mut s = String::new();
    let mut x = seed;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut i = 0;
    while s.len() < n {
        let r = next();
        match r % 5 {
            0 => s.push_str(&format!("## Section {i} {}\n\n", r % 1000)),
            1 => s.push_str(&format!("- item {i} costs ${}.{:02}\n", r % 500, r % 100)),
            2 => s.push_str(&format!("| row {i} | {} |\n", r % 9999)),
            _ => s.push_str(&format!(
                "Paragraph {i}: the quick brown fox {} jumps over the lazy dog {}.\n\n",
                r % 100_000,
                r >> 40
            )),
        }
        i += 1;
    }
    s
}

fn main() {
    let a = make_doc(1, 1 << 20);
    // 约 2% 的行被改动，另有移动
    let mut b: Vec<String> = a.lines().map(String::from).collect();
    for (i, l) in b.iter_mut().enumerate() {
        if i % 50 == 7 && !l.is_empty() {
            l.push_str(" edited");
        }
    }
    let n = b.len();
    b.rotate_left(n / 7);
    let b = b.join("\n");
    let c = make_doc(2, 1 << 20);
    // 词汇完全不同的两份文档
    let d: String = c
        .replace("quick brown fox", "zxq vlm wrt")
        .replace("lazy dog", "kpf mnb")
        .replace("Paragraph", "Entry")
        .replace("Section", "Chapter")
        .replace("item", "thing")
        .replace("row", "line")
        .replace("costs", "sells");
    for (name, x, y) in [
        ("similar+moved", &a, &b),
        ("templated-rewrite", &a, &c),
        ("different-vocab", &a, &d),
        ("identical", &a, &a),
    ] {
        let t = Instant::now();
        let r = diff(x, y);
        println!(
            "{name:17} {:>7.1?}  old={} new={} added={} removed={} modified={} moved={}",
            t.elapsed(),
            r.stats.old_blocks,
            r.stats.new_blocks,
            r.stats.added,
            r.stats.removed,
            r.stats.modified,
            r.stats.moved
        );
    }
}
