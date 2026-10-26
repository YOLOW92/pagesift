//! 拦截页识别的真实网站测试（tasks/0.80/c）：用 pagesift 自己的抓取器（遵守 robots、默认 UA）
//! 逐个抓取，原样存下解码后的 HTML 和状态码，之后离线跑识别，改规则后复测不用再访问网站。
//! 一次一个请求，请求之间停 `PAGESIFT_FETCH_GAP_MS`（默认 2000）毫秒。
//! `raw_fetch <urls.txt> <输出目录>`：每行一个 URL，`#` 开头的行跳过；输出目录里写 `<n>.html` 和 `index.jsonl`。
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use pagesift_policy::{Policy, PolicyConfig, RobotsMode};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: raw_fetch <urls.txt> <out-dir>";
    let list = args.next().expect(usage);
    let out = std::path::PathBuf::from(args.next().expect(usage));
    std::fs::create_dir_all(&out).unwrap();
    let gap = std::env::var("PAGESIFT_FETCH_GAP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let fetcher = Arc::new(pagesift_net::Fetcher::new(Default::default()).unwrap());
    let policy = Policy::new(fetcher, PolicyConfig::default());
    let mut index =
        std::io::BufWriter::new(std::fs::File::create(out.join("index.jsonl")).unwrap());
    let urls: Vec<String> = std::fs::read_to_string(&list)
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    for (n, url) in urls.iter().enumerate() {
        if n > 0 {
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }
        let req = pagesift_net::FetchRequest::new(url.as_str());
        let fetched = tokio::time::timeout(
            Duration::from_secs(60),
            policy.fetch(req, RobotsMode::Respect),
        )
        .await;
        let rec = match fetched {
            Ok(Ok(resp)) => {
                let text = resp.text().text;
                let file = format!("{n}.html");
                std::fs::write(out.join(&file), text.as_bytes()).unwrap();
                serde_json::json!({
                    "n": n, "url": url, "final_url": resp.final_url.as_str(), "status": resp.status,
                    "content_type": resp.content_type(), "bytes": text.len(), "file": file,
                })
            }
            Ok(Err(e)) => serde_json::json!({"n": n, "url": url, "error": e.to_string()}),
            Err(_) => serde_json::json!({"n": n, "url": url, "error": "timeout"}),
        };
        eprintln!("{rec}");
        writeln!(index, "{rec}").unwrap();
        index.flush().unwrap();
    }
}
