//! Spider 整站爬取：每个站点一个 `Website`，全部并发；页面用 Spider 自家的
//! spider_transformations 转成正文 Markdown，和其他工具做同样多的事。
//! 只开基础特性：不启用 UA 伪装、真实浏览器模拟这类反检测功能。
//!
//! `spider_crawl <port> <hosts> <pages> <interval-ms> [per-host]`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use spider::website::Website;
use spider_transformations::transformation::content::{
    ReturnFormat, TransformConfig, transform_content,
};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: spider_crawl <port> <hosts> <pages> <interval-ms> [per-host]";
    let port: u16 = args.next().expect(usage).parse().unwrap();
    let hosts: usize = args.next().expect(usage).parse().unwrap();
    let pages: u32 = args.next().expect(usage).parse().unwrap();
    let interval: u64 = args.next().expect(usage).parse().unwrap();
    let per_host: usize = args.next().map_or(2, |v| v.parse().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let chars = Arc::new(AtomicUsize::new(0));
    let t = Instant::now();
    let tasks = (0..hosts).map(|h| {
        let (count, chars) = (count.clone(), chars.clone());
        tokio::spawn(async move {
            let mut site = Website::new(&format!("http://s{h}.localhost:{port}/"));
            site.with_limit(pages)
                .with_depth(100)
                .with_delay(interval)
                .with_respect_robots_txt(true)
                .with_concurrency_limit(Some(per_host));
            let mut rx = site.subscribe(256);
            let convert = tokio::spawn(async move {
                let cfg = TransformConfig {
                    readability: false,
                    return_format: ReturnFormat::Markdown,
                    filter_images: false,
                    clean_html: false,
                    filter_svg: true,
                    main_content: true,
                };
                loop {
                    let page = match rx.recv().await {
                        Ok(p) => p,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("subscriber lagged by {n} pages");
                            continue;
                        }
                        Err(_) => break,
                    };
                    let md = transform_content(&page, &cfg, &None, &None, &None);
                    count.fetch_add(1, Ordering::Relaxed);
                    chars.fetch_add(md.len(), Ordering::Relaxed);
                }
            });
            site.crawl().await;
            if std::env::var_os("SPIDER_DEBUG").is_some() {
                eprintln!("s{h}: spider visited {} links", site.get_links().len());
            }
            site.unsubscribe();
            let _ = convert.await;
        })
    });
    futures_join(tasks).await;
    let s = t.elapsed().as_secs_f64();
    let n = count.load(Ordering::Relaxed);
    println!(
        "spider: {n} pages from {hosts} sites in {s:.2} s = {:.0} pages/s, markdown {:.1} MB",
        n as f64 / s,
        chars.load(Ordering::Relaxed) as f64 / 1e6
    );
}

async fn futures_join(tasks: impl Iterator<Item = tokio::task::JoinHandle<()>>) {
    for t in tasks.collect::<Vec<_>>() {
        let _ = t.await;
    }
}
