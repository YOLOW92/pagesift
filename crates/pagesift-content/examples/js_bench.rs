use std::time::Instant;
use pagesift_content::scan_script;
use url::Url;

fn main() {
    let base = Url::parse("https://example.com/assets/app.js").unwrap();
    // 构造典型 Webpack/Vite 1MB 打包产物模拟样本
    let snippet = r#"
        import { init } from './vendor.js';
        const API_BASE = '/api/v1';
        const USERS = '/users';
        function getUser(id) { return API_BASE + USERS + '/' + id; }
        const fetchOrders = (token) => fetch(`${API_BASE}/orders?token=${token}`);
        fetch('/api/v1/health');
        location.href = '/dashboard';
        const UNRELATED_DATA = { key: "value", timestamp: 123456789 };
        function compute(a, b) { return a * b + 42; }
    "#;
    let repeats = 1_000_000 / snippet.len();
    let bundle = snippet.repeat(repeats);
    let bytes = bundle.len();
    println!("JS Bundle size: {:.2} MB ({} bytes)", bytes as f64 / 1_000_000.0, bytes);

    // Warmup
    let _ = scan_script(&bundle, &base);

    let iters = 20;
    let t0 = Instant::now();
    for _ in 0..iters {
        let res = scan_script(&bundle, &base);
        std::hint::black_box(res);
    }
    let elapsed = t0.elapsed().as_secs_f64() / iters as f64;
    let ms = elapsed * 1000.0;
    println!("1MB JS Bundle analysis latency: {:.3} ms (Target: < 5ms)", ms);
    assert!(ms < 5.0, "Analysis must be under 5ms!");
}
