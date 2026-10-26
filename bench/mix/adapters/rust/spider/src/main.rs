//! spider（spider-rs）：抓一个 URL（快照回放服务上的地址），转成 Markdown。
//! 抓取用 Website 默认配置，只限一页（with_limit(1)），按 spider README 的订阅方式（subscribe + crawl）拿页面；
//! 2.53.9 的 scrape() + get_pages() 在本机回放服务上拿不到页面（get_pages 为空），所以不用它。
//! 转换用 spider_transformations 的默认 TransformConfig，只把 return_format 设成 Markdown
//! （spider 自己默认返回原始 HTML，Markdown 是它文档里给 LLM 用的格式）。
use spider::website::Website;
use spider_transformations::transformation::content::{transform_content, ReturnFormat, TransformConfig};

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let conf = TransformConfig {
        return_format: ReturnFormat::Markdown,
        ..Default::default()
    };
    mix_proto::serve("spider", "spider 2.53.9", |input| {
        rt.block_on(async {
            let mut site = Website::new(input);
            site.with_limit(1);
            let mut rx = site.subscribe(16);
            let first = tokio::spawn(async move { rx.recv().await.ok() });
            site.crawl().await;
            site.unsubscribe();
            let page = first
                .await
                .ok()
                .flatten()
                .ok_or_else(|| "no page".to_string())?;
            let status = page.status_code.as_u16();
            if status >= 400 || page.get_html_bytes_u8().is_empty() {
                return Err(format!("http status {status}, empty body"));
            }
            Ok(transform_content(&page, &conf, &None, &None, &None))
        })
    });
}
