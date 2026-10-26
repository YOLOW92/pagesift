//! Google robots.txt 规范测试驱动器。
//!
//! 读 cases.jsonl（每行 {file, robotstxt, url, useragent, expected}），
//! 用 pagesift-policy 的 RFC 9309 实现判定，输出 {index, got, expected, pass}。
//! ALLOWED / DISALLOWED 之外还可能出现 BYTES 等类型，这里只处理允许/禁止两类。

use std::io::{BufRead, BufReader, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("usage: pagesift-robots-spec <cases.jsonl> <out.jsonl>")?;
    let output = args.next().ok_or("usage: pagesift-robots-spec <cases.jsonl> <out.jsonl>")?;

    let reader = BufReader::new(std::fs::File::open(&input)?);
    let mut out = std::io::BufWriter::new(std::fs::File::create(&output)?);
    let (mut pass, mut total, mut skipped) = (0usize, 0usize, 0usize);

    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line)?;
        let expected = row["expected"].as_str().unwrap_or("");
        // 只评允许/禁止；其它期望类型（如字节级比较）不适用本实现
        let want = match expected {
            "ALLOWED" => true,
            "DISALLOWED" => false,
            _ => {
                skipped += 1;
                writeln!(out, "{}", serde_json::json!({
                    "index": i, "file": row["file"], "skipped": true, "expected": expected
                }))?;
                continue;
            }
        };
        let robots = pagesift_policy::Robots::parse(row["robotstxt"].as_str().unwrap_or(""));
        let ua = row["useragent"].as_str().unwrap_or("");
        let path = url::Url::parse(row["url"].as_str().unwrap_or(""))
            .map(|u| {
                let mut p = u.path().to_string();
                if let Some(q) = u.query() {
                    p.push('?');
                    p.push_str(q);
                }
                p
            })
            .unwrap_or_else(|_| "/".into());
        let got = robots.is_allowed(ua, &path);
        total += 1;
        if got == want {
            pass += 1;
        }
        writeln!(out, "{}", serde_json::json!({
            "index": i,
            "file": row["file"],
            "url": row["url"],
            "useragent": ua,
            "expected": expected,
            "got": if got { "ALLOWED" } else { "DISALLOWED" },
            "pass": got == want
        }))?;
    }
    eprintln!("total={total} pass={pass} skipped={skipped} rate={:.4}",
        if total > 0 { pass as f64 / total as f64 } else { 0.0 });
    Ok(())
}
