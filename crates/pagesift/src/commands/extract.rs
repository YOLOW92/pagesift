//! `pagesift extract --schema schema.json URL...`：按 CSS schema 批量抽取，每个网址输出一行 JSON。
//!
//! 和 HTTP 接口的 `{"type": "extract", "schema": ...}` 格式是同一条路径：默认打开存储（`[cache]`），
//! 选择器有记忆，页面改版后自动重定位，在 `warnings` 里报 `relocated`。页面本身不走缓存，每次都抓新的。
//!
//! `pagesift similar URL --selector CSS`：给一个例子，找出结构相同的兄弟元素。

use crate::*;
use futures_util::StreamExt;

pub(crate) struct ExtractArgs {
    pub urls: Vec<String>,
    pub schema: std::path::PathBuf,
    pub engine: String,
    pub ignore_robots: bool,
    pub concurrency: Option<usize>,
    pub allow: AllowCidrs,
}

pub(crate) struct SimilarArgs {
    pub url: String,
    pub selector: String,
    pub engine: String,
    pub ignore_robots: bool,
    pub allow: AllowCidrs,
}

/// 按配置打开存储，返回打开了没有。数据目录被别的进程占着时警告后继续（没有缓存和选择器记忆）。
pub(crate) fn attach_store(
    cfg: &pagesift::config::Config,
    sc: &mut pagesift::ScraperConfig,
) -> Result<bool> {
    if !cfg.cache.enabled.unwrap_or(true) {
        return Ok(false);
    }
    match cfg.open_store() {
        Ok(store) => {
            sc.store = Some(store);
            Ok(true)
        }
        Err(e)
            if e.downcast_ref::<pagesift_store::Error>()
                .is_some_and(pagesift_store::Error::is_locked) =>
        {
            eprintln!("warning: {e:#}; running without the cache and selector memory");
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

pub(crate) fn run_similar(args: SimilarArgs, cfg: pagesift::config::Config) -> Result<ExitCode> {
    let opts = pagesift::api::types::ScrapeOptions {
        formats: vec![pagesift::api::types::Format::new("rawHtml")],
        ignore_robots_txt: args.ignore_robots,
        engine: Some(args.engine),
        ..Default::default()
    };
    light_runtime()?.block_on(async move {
        let mut sc = cfg.scraper(false)?;
        args.allow.apply(&mut sc);
        let scraper = pagesift::Scraper::new(sc)?;
        let r = scraper.find_similar(&args.url, &args.selector, &opts).await;
        scraper.shutdown().await;
        match r {
            Ok(data) => {
                println!("{}", serde_json::to_string(&data)?);
                Ok(ExitCode::SUCCESS)
            }
            Err(e) => {
                eprintln!("{}: {}", e.code.as_str(), e.message);
                Ok(ExitCode::from(1))
            }
        }
    })
}

pub(crate) fn run(args: ExtractArgs, cfg: pagesift::config::Config) -> Result<ExitCode> {
    let schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&args.schema)
            .with_context(|| format!("reading {}", args.schema.display()))?,
    )
    .with_context(|| format!("parsing {}", args.schema.display()))?;
    anyhow::ensure!(schema.is_object(), "the schema must be a JSON object");
    let mut format = pagesift::api::types::Format::new("extract");
    format.options.insert("schema".into(), schema);
    let opts = pagesift::api::types::ScrapeOptions {
        formats: vec![format],
        ignore_robots_txt: args.ignore_robots,
        engine: Some(args.engine),
        max_age: Some(0),
        store_in_cache: false,
        ..Default::default()
    };
    light_runtime()?.block_on(async move {
        let mut sc = cfg.scraper(false)?;
        attach_store(&cfg, &mut sc)?;
        args.allow.apply(&mut sc);
        let scraper = pagesift::Scraper::new(sc)?;
        // 没给并发时取统一准入（限速器的 USL 控制器）当前的在途目标，和批量抓取一致
        let admitted = pagesift::api::Engine::admitted_concurrency(&scraper).unwrap_or(1);
        let n = args.concurrency.unwrap_or(admitted).max(1);
        let mut failed = false;
        let mut results = futures_util::stream::iter(args.urls)
            .map(|url| {
                let (scraper, opts) = (&scraper, &opts);
                async move {
                    let r = scraper.scrape(&url, opts).await;
                    (url, r)
                }
            })
            .buffered(n);
        while let Some((url, r)) = results.next().await {
            let line = match r {
                Ok(doc) => serde_json::json!({
                    "url": url,
                    "success": true,
                    "extract": doc.extract,
                    "warnings": doc.metadata.pagesift.map(|m| m.warnings).unwrap_or_default(),
                }),
                Err(e) => {
                    failed = true;
                    serde_json::json!({
                        "url": url,
                        "success": false,
                        "error": { "code": e.code.as_str(), "message": e.message },
                    })
                }
            };
            println!("{line}");
        }
        drop(results);
        scraper.shutdown().await;
        Ok(if failed {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    })
}
