//! stable 模糊测试（`pagesift-fuzz pdf`，见 scripts/fuzz.sh）找到的用例，最小化后放在这里。

use std::time::{Duration, Instant};

use pagesift_pdf::{Options, fuzzing};

fn quick(data: &[u8]) -> Duration {
    let opts = Options {
        timeout: Some(Duration::from_secs(10)),
        ..Options::default()
    };
    let t = Instant::now();
    let _ = fuzzing::extract_unguarded(data, &opts);
    t.elapsed()
}

/// 修复扫描里每个没有 endstream 的 `stream` 关键字都从当前位置找到文件末尾：
/// 63 万字节、大量 `stream\n` 的输入曾经要 20 秒（而且不受 `timeout` 约束）。
#[test]
fn many_streams_without_endstream() {
    let mut data = b"%PDF-1.4\n1 0 obj\n<< /Length 5 >>\n".to_vec();
    data.extend(b"stream\n".repeat(100_000));
    let took = quick(&data);
    assert!(took < Duration::from_secs(2), "{took:?}");
}
