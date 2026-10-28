//! 渲染分阶段耗时。手动运行：
//! `cargo test -p pagesift-cdp --release --test bench -- --ignored --nocapture`

mod common;

use std::time::Duration;

use common::*;
use pagesift_cdp::*;

const RUNS: usize = 7;

fn route(r: &Req) -> Resp {
    match r.path.as_str() {
        // 文章页：20 张图片和一个样式表都有 CDN 延迟。
        "/article" => html(&format!(
            r#"<link rel="stylesheet" href="/s.css"><article><h1>Article</h1>{}</article>"#,
            (0..20)
                .map(|i| format!(r#"<p>paragraph {i} of the article body.</p><img src="/img{i}.png">"#))
                .collect::<String>()
        )),
        "/s.css" => Resp {
            ctype: "text/css",
            ..html_raw("body { margin: 0 }")
        }
        .delayed(100),
        // SPA：脚本加载后请求接口再渲染。
        "/spa" => html(r#"<div id="app">loading</div><script src="/app.js"></script>"#),
        "/app.js" => js(
            "fetch('/api').then(r => r.text()).then(t => document.getElementById('app').textContent = t);",
        )
        .delayed(50),
        "/api" => Resp {
            ctype: "text/plain",
            ..html_raw("spa content ready")
        }
        .delayed(150),
        "/storybook" => html(r#"<nav>manager</nav><iframe src="/preview"></iframe>"#),
        "/preview" => html(r#"<div id="root"></div><script src="/app.js"></script><div id="app"></div>"#),
        // 大 DOM：5000 段落加 50 个 shadow 组件。
        "/big" => html(&format!(
            r#"{}{}<script>customElements.define('x-c', class extends HTMLElement {{ constructor() {{ super(); this.attachShadow({{mode:'open'}}).innerHTML = '<b>card</b><slot></slot>'; }} }});</script>"#,
            (0..5000).map(|i| format!("<p>row {i} <a href=\"/x{i}\">link</a></p>")).collect::<String>(),
            (0..50).map(|i| format!("<x-c><i>c{i}</i></x-c>")).collect::<String>()
        )),
        p if p.ends_with(".png") => not_found().delayed(300),
        _ => html("<p>default</p>"),
    }
}

fn median(mut v: Vec<Duration>) -> f64 {
    v.sort();
    v[v.len() / 2].as_secs_f64() * 1000.0
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn phase_timings() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    // 预热：启动浏览器。
    pool.render(&format!("{base}/article"), &opts())
        .await
        .unwrap();
    let pages = [
        ("article", "/article", "paragraph 19"),
        ("spa", "/spa", "spa content ready"),
        ("storybook", "/storybook", "spa content ready"),
        ("big-dom", "/big", "row 4999"),
    ];
    println!("| page | ok | setup | navigation | wait | html | iframes | total |");
    println!("|---|---|---|---|---|---|---|---|");
    for (name, path, expect) in pages {
        let mut cols: Vec<Vec<Duration>> = vec![Vec::new(); 6];
        let mut ok = 0;
        for _ in 0..RUNS {
            let p = pool
                .render(&format!("{base}{path}"), &opts())
                .await
                .unwrap();
            ok += p.html.contains(expect) as usize;
            // wall-clock: a benchmark that prints the timings, nothing is asserted on them
            let t = &p.timings;
            for (c, d) in
                cols.iter_mut()
                    .zip([t.setup, t.navigation, t.wait, t.html, t.iframes, t.total])
            {
                c.push(d);
            }
        }
        let m: Vec<String> = cols
            .into_iter()
            .map(|c| format!("{:.1}", median(c)))
            .collect();
        println!("| {name} | {ok}/{RUNS} | {} |", m.join(" | "));
    }
    shutdown(pool).await;
}
