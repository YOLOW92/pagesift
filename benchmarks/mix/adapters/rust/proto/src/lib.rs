//! 适配器协议（见 benchmarks/mix/adapters/README.md）：
//! 启动后先在 stdout 打一行 `{"ready":true,...}`；之后每从 stdin 读到一行 `{"id","input","out"}`，
//! 就把结果写进 `out`，并在 stdout 回一行 `{"id","ok","ms","bytes","error"}`。超时、内存由外层 driver 管。
//! 任务行可以带可选的 `url`（网页快照的原始地址），需要的工具用 `serve_with_url`。
use std::io::{BufRead, Write};
use std::time::Instant;

pub fn serve<F>(tool: &str, version: &str, mut convert: F)
where
    F: FnMut(&str) -> Result<String, String>,
{
    serve_with_url(tool, version, |input, _url| convert(input))
}

pub fn serve_with_url<F>(tool: &str, version: &str, mut convert: F)
where
    F: FnMut(&str, Option<&str>) -> Result<String, String>,
{
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let ready = serde_json::json!({"ready": true, "tool": tool, "version": version, "pid": std::process::id()});
    writeln!(out, "{ready}").unwrap();
    out.flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let job: serde_json::Value = serde_json::from_str(&line).expect("bad job line");
        let id = job["id"].clone();
        let input = job["input"].as_str().unwrap_or_default();
        let target = job["out"].as_str().unwrap_or_default();
        let url = job["url"].as_str();
        let t = Instant::now();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert(input, url)))
            .unwrap_or_else(|e| {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                Err(format!("panic: {msg}"))
            });
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let reply = match res {
            Ok(text) => {
                std::fs::write(target, &text).expect("write output");
                serde_json::json!({"id": id, "ok": true, "ms": ms, "bytes": text.len(), "error": null})
            }
            Err(e) => serde_json::json!({"id": id, "ok": false, "ms": ms, "bytes": 0, "error": e}),
        };
        writeln!(out, "{reply}").unwrap();
        out.flush().unwrap();
    }
}

/// 网页快照可能不是 UTF-8；各工具拿到的都是同一份原始字节，这里按 UTF-8 宽松解码。
pub fn read_lossy(path: &str) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|e| e.to_string())
}
