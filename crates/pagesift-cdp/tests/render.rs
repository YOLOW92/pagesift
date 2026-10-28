//! iframe 合并、Shadow DOM、整页滚动、虚拟滚动、actions 和截图的真实浏览器测试。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use pagesift_cdp::*;

fn route(r: &Req) -> Resp {
    let other = if r.host == "localhost" {
        format!("http://127.0.0.1:{}", r.port)
    } else {
        format!("http://localhost:{}", r.port)
    };
    match r.path.as_str() {
        // Storybook：管理界面在主文档，正文只在 preview iframe 里由脚本渲染。
        "/sb" => html(
            r#"<nav id="root">Storybook manager sidebar</nav>
               <iframe id="storybook-preview-iframe" title="storybook-preview-iframe" src="/sb/iframe.html?id=button--primary"></iframe>"#,
        ),
        "/sb-cross" => html(&format!(
            r#"<nav>manager</nav><iframe id="storybook-preview-iframe" src="{other}/sb/iframe.html?id=button--primary"></iframe>"#
        )),
        "/sb/iframe.html" => html(r#"<div id="storybook-root"></div><script src="/sb/main.js"></script>"#),
        "/sb/main.js" => js(
            r#"fetch('/sb/story.json').then(r => r.json()).then(s => {
                 document.getElementById('storybook-root').innerHTML =
                   '<h1>' + s.title + '</h1><p>' + s.body + '</p><img src="/secret-sb.png">';
               });"#,
        )
        .delayed(100),
        "/sb/story.json" => Resp {
            ctype: "application/json",
            ..html_raw(r#"{"title":"Button story","body":"Primary buttons carry the main call to action."}"#)
        }
        .delayed(150),
        // 无限嵌套：每层换一次站点，跨进程 iframe 里再套跨进程 iframe。
        "/nest" => {
            let d: u32 = r.param("d").and_then(|d| d.parse().ok()).unwrap_or(0);
            let child = if d < 12 {
                format!(r#"<iframe src="{other}/nest?d={}"></iframe>"#, d + 1)
            } else {
                String::new()
            };
            html(&format!("<p>level-{d}</p>{child}"))
        }
        "/many" => html(
            &(0..5)
                .map(|i| format!(r#"<iframe srcdoc="<p>sibling-{i}</p>"></iframe>"#))
                .collect::<String>(),
        ),
        "/blocked-frame" => html(r#"<p>outer</p><iframe src="/secret-frame"></iframe>"#),
        "/shadow" => html(
            r#"<x-card><span>light child</span></x-card>
               <x-closed></x-closed>
               <script>
                 customElements.define('x-card', class extends HTMLElement {
                   constructor() { super(); this.attachShadow({mode: 'open'}).innerHTML =
                     '<h2>shadow title</h2><slot></slot><x-inner></x-inner><p>after &amp; slot</p><iframe srcdoc="<p>frame in shadow</p>"></iframe>'; }
                 });
                 customElements.define('x-inner', class extends HTMLElement {
                   constructor() { super(); this.attachShadow({mode: 'open'}).innerHTML = '<em>nested shadow</em><slot>fallback text</slot>'; }
                 });
                 customElements.define('x-closed', class extends HTMLElement {
                   constructor() { super(); this.attachShadow({mode: 'closed'}).innerHTML = '<b>closed secret</b>'; }
                 });
               </script>"#,
        ),
        // 滚到底才加载下一页，共 5 页。
        "/infinite" => html(
            r#"<div id="feed"></div><div id="sentinel" style="height:10px"></div>
               <script>
                 let page = 0, busy = false;
                 const more = () => {
                   if (busy || page >= 5) return; busy = true;
                   fetch('/items?page=' + page).then(r => r.text()).then(t => {
                     const d = document.createElement('div'); d.style.height = '1500px'; d.textContent = t;
                     document.getElementById('feed').appendChild(d); page++; busy = false;
                   });
                 };
                 more();
                 new IntersectionObserver(es => { if (es.some(e => e.isIntersecting)) more(); }).observe(document.getElementById('sentinel'));
                 window.addEventListener('scroll', () => { if (innerHeight + scrollY >= document.body.scrollHeight - 20) more(); });
               </script>"#,
        ),
        "/items" => Resp {
            ctype: "text/plain",
            ..html_raw(&format!("feed-page-{}", r.param("page").unwrap_or("?")))
        }
        .delayed(120),
        // 只渲染可见的 10 行，滚动时回收节点。
        "/virtual" => html(
            r#"<div id="list" style="height:300px; overflow-y:auto; position:relative">
                 <div id="spacer" style="height:3000px"></div>
               </div>
               <script>
                 const list = document.getElementById('list');
                 const render = () => {
                   const first = Math.floor(list.scrollTop / 30);
                   list.querySelectorAll('.row').forEach(r => r.remove());
                   for (let i = first; i < Math.min(first + 10, 100); i++) {
                     const r = document.createElement('div'); r.className = 'row';
                     r.style.cssText = 'position:absolute; height:30px; top:' + (i * 30) + 'px';
                     r.textContent = 'row ' + i; list.appendChild(r);
                   }
                 };
                 list.addEventListener('scroll', render); render();
               </script>"#,
        ),
        // 不发请求、用定时器延后渲染：网络一开始就静默，只有"loading…"的壳
        "/late" => html(
            r#"<div id="c">loading…</div>
               <script>setTimeout(() => { document.getElementById('c').textContent =
                 'Rendered after a timer. '.repeat(20); }, 1000);</script>"#,
        ),
        "/tall" => html(r#"<div style="height:4000px; background:linear-gradient(red, blue)">tall</div>"#),
        // 图片很慢，正文由 fetch 渲染。
        "/slow-image" => html(
            r#"<div id="c">loading</div><img src="/slow.png">
               <script>fetch('/data').then(r => r.text()).then(t => document.getElementById('c').textContent = t);</script>"#,
        ),
        "/data" => Resp {
            ctype: "text/plain",
            ..html_raw("data from api")
        }
        .delayed(200),
        // 比下面"不等它"的上限（3 s）慢得多，忙的机器上也分得清等没等
        "/slow.png" => not_found().delayed(4000),
        // 一直轮询的页面：正文另外由一次接口请求渲染。
        "/poll" => html(
            r#"<div id="c">loading</div>
               <script>
                 setInterval(() => fetch('/tick').catch(() => {}), 50);
                 fetch('/data').then(r => r.text()).then(t => document.getElementById('c').textContent = t);
               </script>"#,
        ),
        "/tick" => html_raw("{}").delayed(30),
        // blob: 主脚本的 worker。
        "/worker" => html(
            r#"<p>page with a worker</p>
               <script>new Worker(URL.createObjectURL(new Blob(['postMessage(1)'], {type: 'text/javascript'})));</script>"#,
        ),
        // 解析到一半就跳到连不上的地址：原文档等不到 DOMContentLoaded。
        "/js-redirect" => html_raw(
            r#"<!doctype html><html><head><script>location.replace('http://127.0.0.1:1/');</script></head>
               <body><p>original</p><script>for (const t = Date.now(); Date.now() - t < 300;) {}</script></body></html>"#,
        ),
        p if p.ends_with(".png") || p.starts_with("/secret") => not_found(),
        _ => html("<p>default</p>"),
    }
}

fn count(h: &str, needle: &str) -> usize {
    h.matches(needle).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn storybook_iframe_content_is_inlined() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), Arc::new(DenySubstrings(vec!["/secret"])))
        .await
        .unwrap();
    for path in ["/sb", "/sb-cross"] {
        let page = pool
            .render(&format!("{base}{path}"), &opts())
            .await
            .unwrap();
        let h = &page.html;
        assert!(
            h.contains("Primary buttons carry the main call to action."),
            "{path}: {h}"
        );
        assert!(h.contains("data-pagesift-iframe=\""), "{path}: {h}");
        assert!(h.contains("/sb/iframe.html?id=button--primary\">"), "{h}");
        assert!(!h.contains("<iframe"), "{path}: iframe left: {h}");
        assert!(!h.contains("<!--pagesift-"), "{path}: marker leaked: {h}");
        assert_eq!((page.iframes_inlined, page.iframes_skipped), (1, 0));
        // iframe 里的子请求同样过了 guard。
        assert!(
            page.blocked_requests
                .iter()
                .any(|b| b.url.ends_with("/secret-sb.png")),
            "{path}: {:?}",
            page.blocked_requests
        );
        // wall-clock: compares two parts of one measurement with each other, not with a fixed time
        assert!(page.timings.iframes <= page.timings.html);
    }
    // 关掉后保留原始 iframe，不含正文。
    let page = pool
        .render(
            &format!("{base}/sb"),
            &RenderOptions {
                process_iframes: false,
                ..opts()
            },
        )
        .await
        .unwrap();
    assert!(
        page.html
            .contains("<iframe id=\"storybook-preview-iframe\"")
    );
    assert!(!page.html.contains("Primary buttons"));
    assert_eq!(page.iframes_inlined, 0);
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn iframe_depth_and_count_are_limited() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), Arc::new(DenySubstrings(vec!["/secret"])))
        .await
        .unwrap();
    let page = pool
        .render(
            &format!("{base}/nest?d=0"),
            &RenderOptions {
                max_iframe_depth: 3,
                ..opts()
            },
        )
        .await
        .unwrap();
    for d in 0..=3 {
        assert!(page.html.contains(&format!("level-{d}<")), "{}", page.html);
    }
    assert!(!page.html.contains("level-4"), "{}", page.html);
    assert_eq!(page.iframes_inlined, 3);
    assert_eq!(page.iframes_skipped, 1);
    // 第 4 层的 iframe 原样保留。
    assert_eq!(count(&page.html, "<iframe"), 1, "{}", page.html);
    assert_eq!(count(&page.html, "data-pagesift-iframe"), 3);

    let page = pool
        .render(
            &format!("{base}/many"),
            &RenderOptions {
                max_iframes: 2,
                ..opts()
            },
        )
        .await
        .unwrap();
    assert_eq!((page.iframes_inlined, page.iframes_skipped), (2, 3));
    assert!(page.html.contains("<p>sibling-0</p>"));
    assert!(page.html.contains("<p>sibling-1</p>"));
    assert!(!page.html.contains("<p>sibling-2</p>"));
    assert_eq!(count(&page.html, "<iframe"), 3);
    assert!(page.html.contains(r#"data-pagesift-iframe="about:srcdoc""#));

    // 被 guard 拦下的 iframe 是错误页，不内联，原样保留。
    let page = pool
        .render(&format!("{base}/blocked-frame"), &opts())
        .await
        .unwrap();
    assert_eq!((page.iframes_inlined, page.iframes_skipped), (0, 1));
    assert!(
        page.html.contains(r#"<iframe src="/secret-frame">"#),
        "{}",
        page.html
    );
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn open_shadow_roots_are_flattened() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let page = pool
        .render(&format!("{base}/shadow"), &opts())
        .await
        .unwrap();
    let h = &page.html;
    let pos = |s: &str| h.find(s).unwrap_or_else(|| panic!("missing {s}: {h}"));
    // 组合树顺序：shadow 内容、slot 里的 light 子节点、嵌套 shadow、slot 之后的内容。
    assert!(pos("<h2>shadow title</h2>") < pos("<span>light child</span>"));
    assert!(pos("<span>light child</span>") < pos("<em>nested shadow</em>"));
    assert!(pos("<em>nested shadow</em>") < pos("fallback text"));
    assert!(pos("fallback text") < pos("<p>after &amp; slot</p>"));
    assert_eq!(count(h, "light child"), 1);
    assert!(!h[..pos("<script>")].contains("<slot"), "{h}");
    assert!(h.contains("<p>frame in shadow</p>"), "{h}");
    assert!(!h[..pos("<script>")].contains("closed secret"));
    assert!(h.contains("<script>"), "scripts are kept verbatim");

    let page = pool
        .render(
            &format!("{base}/shadow"),
            &RenderOptions {
                flatten_shadow_dom: false,
                ..opts()
            },
        )
        .await
        .unwrap();
    let body = page.html.split("<script>").next().unwrap();
    assert!(!body.contains("shadow title"), "{body}");
    assert!(page.html.contains("<span>light child</span>"));
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn full_page_scan_triggers_lazy_loading() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let url = format!("{base}/infinite");
    let plain = pool.render(&url, &opts()).await.unwrap();
    assert!(plain.html.contains("feed-page-0"));
    assert!(!plain.html.contains("feed-page-2"));

    let page = pool
        .render(
            &url,
            &RenderOptions {
                scan_full_page: Some(FullPageScan::default()),
                ..opts()
            },
        )
        .await
        .unwrap();
    for i in 0..5 {
        assert!(
            page.html.contains(&format!("feed-page-{i}")),
            "{}",
            page.html
        );
    }
    // wall-clock: a scan that never sees the end of the feed runs to its 10 s timeout; 8 s is
    // below that and far above the five scroll steps it takes
    assert!(
        page.timings.wait < Duration::from_secs(8),
        "{:?}",
        page.timings
    );
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn virtual_scroll_collects_recycled_rows() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let url = format!("{base}/virtual");
    let plain = pool.render(&url, &opts()).await.unwrap();
    assert_eq!(count(&plain.html, "class=\"row\""), 10);

    let page = pool
        .render(
            &url,
            &RenderOptions {
                virtual_scroll: Some(VirtualScroll {
                    scroll_count: 30,
                    wait_after_scroll: Duration::from_millis(300),
                    ..VirtualScroll::new("#list")
                }),
                ..opts()
            },
        )
        .await
        .unwrap();
    // 100 行加上占高度的 spacer。
    assert_eq!(page.virtual_scroll_items, Some(101));
    let h = &page.html;
    let mut last = 0;
    for i in 0..100 {
        let needle = format!(">row {i}<");
        assert_eq!(count(h, &needle), 1, "row {i}: {h}");
        let p = h.find(&needle).unwrap();
        assert!(p > last, "row {i} out of order");
        last = p;
    }
    assert!(!h.contains("data-pagesift-vs"));

    let missing = pool
        .render(
            &url,
            &RenderOptions {
                virtual_scroll: Some(VirtualScroll::new("#nope")),
                ..opts()
            },
        )
        .await
        .unwrap();
    assert_eq!(missing.virtual_scroll_items, None);
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn firecrawl_actions_and_screenshot_formats() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(
        PoolConfig {
            allow_execute_javascript: true,
            ..config()
        },
        allow_all(),
    )
    .await
    .unwrap();
    let actions = parse_actions(&serde_json::json!([
        {"type": "wait", "milliseconds": 50},
        {"type": "scroll", "direction": "down"},
        {"type": "executeJavascript", "script": "window.scrollY > 0"},
        {"type": "screenshot", "fullPage": true},
        {"type": "screenshot", "quality": 40, "viewport": {"width": 640, "height": 480}},
        {"type": "scrape"},
        {"type": "pdf", "format": "A4", "landscape": true}
    ]))
    .unwrap();
    let page = pool
        .render(
            &format!("{base}/tall"),
            &RenderOptions {
                actions,
                screenshot: Some(ScreenshotOptions {
                    full_page: false,
                    quality: None,
                    viewport: Some(Viewport {
                        width: 800,
                        height: 600,
                    }),
                }),
                ..opts()
            },
        )
        .await
        .unwrap();
    let a = &page.actions;
    assert_eq!(a.javascript_returns[0].value, serde_json::json!(true));
    let (w, h) = png_size(&a.screenshots[0].data);
    assert_eq!(w, 1920);
    assert!(h >= 4000, "full page height {h}");
    assert_eq!(a.screenshots[1].format, ImageFormat::Jpeg);
    assert!(a.screenshots[1].data.starts_with(&[0xFF, 0xD8]));
    let shot = page.screenshot.as_ref().unwrap();
    assert_eq!(png_size(&shot.data), (800, 600));

    let j = a.to_firecrawl_json();
    assert!(
        j["screenshots"][0]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,iVBOR")
    );
    assert!(
        j["screenshots"][1]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    assert!(j["scrapes"][0]["html"].as_str().unwrap().contains("tall"));
    assert_eq!(j["scrapes"][0]["url"], format!("{base}/tall"));
    assert_eq!(
        j["javascriptReturns"][0],
        serde_json::json!({"type": "boolean", "value": true})
    );
    assert!(
        j["pdfs"][0]
            .as_str()
            .unwrap()
            .starts_with("data:application/pdf;base64,JVBER")
    );
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_idle_does_not_wait_for_slow_images() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let url = format!("{base}/slow-image");
    let fast = pool.render(&url, &opts()).await.unwrap();
    assert!(fast.html.contains("data from api"), "{}", fast.html);
    // wall-clock: the image takes 4 s, the data 0.2 s. Lower bounds (load, screenshot) only need
    // the server's delay to have happened; the 3 s upper bound sits below the 4 s the bug waits
    assert!(
        fast.timings.navigation + fast.timings.wait < Duration::from_secs(3),
        "{:?}",
        fast.timings
    );
    let load = pool
        .render(
            &url,
            &RenderOptions {
                wait_until: WaitUntil::Load,
                ..opts()
            },
        )
        .await
        .unwrap();
    // wall-clock: lower bound, the 4 s image delay is on the server
    assert!(
        load.timings.navigation >= Duration::from_secs(2),
        "{:?}",
        load.timings
    );
    // 要截图时仍然等 load，图片才画得出来。
    let shot = pool
        .render(
            &url,
            &RenderOptions {
                screenshot: Some(ScreenshotOptions::default()),
                ..opts()
            },
        )
        .await
        .unwrap();
    // wall-clock: lower bound, the 4 s image delay is on the server
    assert!(
        shot.timings.screenshot >= Duration::from_millis(1000),
        "{:?}",
        shot.timings
    );
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_that_renders_after_a_timer_is_waited_for() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    let page = pool.render(&format!("{base}/late"), &opts()).await.unwrap();
    // 脚本源码里也有那句话，所以看占位的"loading…"有没有被换掉
    assert!(!page.html.contains("loading…"), "{}", page.html);
    shutdown(pool).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn readiness_ignores_polling_workers_and_abandoned_documents() {
    require_browser!();
    let (base, _) = serve(route).await;
    let pool = BrowserPool::new(config(), allow_all()).await.unwrap();
    // 轮询永远不停，但同一个地址重复请求不算未完成的加载；正文接口仍然要等到。
    // 判错时会等满网络空闲的上限（5 s），上限 4 s 给忙的机器留余量。
    let poll = pool.render(&format!("{base}/poll"), &opts()).await.unwrap();
    assert!(poll.html.contains("data from api"), "{}", poll.html);
    // wall-clock: see above, the bug waits the 5 s network-idle limit
    assert!(
        poll.timings.wait < Duration::from_secs(4),
        "{:?}",
        poll.timings
    );
    let worker = pool
        .render(&format!("{base}/worker"), &opts())
        .await
        .unwrap();
    assert!(worker.html.contains("page with a worker"));
    // wall-clock: the bug waits the 5 s network-idle limit
    assert!(
        worker.timings.wait < Duration::from_secs(4),
        "{:?}",
        worker.timings
    );
    // 以前会一直等原文档的 DOMContentLoaded，直到整体超时；现在很快报导航失败，
    // 不把浏览器的错误页当正文返回。
    // wall-clock: the bug waits for the 30 s render timeout; 15 s is far above the normal time
    let start = std::time::Instant::now();
    let redirect = pool.render(&format!("{base}/js-redirect"), &opts()).await;
    assert!(
        matches!(redirect, Err(Error::Navigation(_))),
        "{redirect:?}"
    );
    // 判错时要等到整体超时（30 s）
    assert!(start.elapsed() < Duration::from_secs(15));
    shutdown(pool).await;
}
