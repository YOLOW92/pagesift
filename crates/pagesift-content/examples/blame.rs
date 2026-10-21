//! 诊断：评测短语在只取正文时被哪条规则删掉 / 保留在哪里。
//! 输入是 JSON lines：`{"id", "path", "phrases": [...]}`，输出每个短语一行：id、原因、元素、短语。
//! `cargo run --release -p pagesift-content --example blame -- phrases.jsonl`

use std::io::BufRead;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: blame <phrases.jsonl>");
    for line in std::io::BufReader::new(std::fs::File::open(path).unwrap()).lines() {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let html = String::from_utf8_lossy(&std::fs::read(v["path"].as_str().unwrap()).unwrap())
            .into_owned();
        let phrases: Vec<String> = v["phrases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect();
        for (p, r) in phrases
            .iter()
            .zip(pagesift_content::explain_phrases(&html, &phrases))
        {
            let (reason, el) = match r {
                None => ("not-found", String::new()),
                Some((None, chain)) => ("kept", chain),
                Some((Some(r), el)) => (r, el),
            };
            println!(
                "{}\t{}\t{}\t{}",
                v["id"].as_str().unwrap(),
                reason,
                el,
                p.chars().take(80).collect::<String>()
            );
        }
    }
}
