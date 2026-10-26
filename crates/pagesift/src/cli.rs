//! CLI 辅助：crawl 的输出和 doctor 环境检查。

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::Result;
use pagesift::api::types::{CrawlSummary, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};

/// crawl 的结果：有输出目录时每页一个 .md 文件，加一个 index.jsonl；否则每页一行 JSON 打到标准输出。
pub struct CrawlOutput {
    dir: Option<PathBuf>,
    state: Mutex<(usize, usize, std::collections::HashSet<String>)>,
}

impl CrawlOutput {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            state: Mutex::new((0, 0, Default::default())),
        }
    }

    pub fn counts(&self) -> (usize, usize) {
        let s = self.state.lock().expect("lock");
        (s.0, s.1)
    }

    fn file_name(&self, url: &str, used: &mut std::collections::HashSet<String>) -> String {
        let trimmed = url.split_once("://").map_or(url, |(_, rest)| rest);
        let mut name: String = trimmed
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        name.truncate(150);
        let name = name.trim_matches('_').to_string();
        let mut candidate = format!("{name}.md");
        let mut n = 2;
        while !used.insert(candidate.clone()) {
            candidate = format!("{name}-{n}.md");
            n += 1;
        }
        candidate
    }

    fn write_page(&self, doc: &Document) -> std::io::Result<()> {
        let url = doc
            .metadata
            .source_url
            .clone()
            .or(doc.metadata.url.clone())
            .unwrap_or_default();
        let mut state = self.state.lock().expect("lock");
        state.0 += 1;
        if let Some(warnings) = doc.metadata.pagesift.as_ref().map(|p| &p.warnings) {
            for warning in warnings {
                if warning.starts_with("crawl_followed_zero:")
                    || warning.starts_with("crawl_scope:")
                {
                    eprintln!("warning: {warning}");
                }
            }
        }
        match &self.dir {
            Some(dir) => {
                let name = self.file_name(&url, &mut state.2);
                std::fs::write(dir.join(&name), doc.markdown.as_deref().unwrap_or_default())?;
                let line =
                    serde_json::json!({ "url": url, "file": name, "title": doc.metadata.title });
                let mut index = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("index.jsonl"))?;
                writeln!(index, "{line}")?;
                eprintln!("{url} -> {name}");
            }
            None => {
                let line = serde_json::json!({ "url": url, "title": doc.metadata.title, "markdown": doc.markdown });
                println!("{line}");
            }
        }
        Ok(())
    }
}

impl CrawlSink for CrawlOutput {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        if let Err(e) = self.write_page(&doc) {
            eprintln!("could not write page: {e}");
        }
        Box::pin(async {})
    }

    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.state.lock().expect("lock").1 += 1;
        eprintln!("{url}: {} {}", err.code.as_str(), err.message);
        Box::pin(async {})
    }

    fn total(&self, _total: usize) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn cancelled(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn summary(&self, s: CrawlSummary) -> BoxFuture<'_, ()> {
        match s.coverage {
            Some(c) => eprintln!("stopped: {} (coverage {c:.2})", s.stop_reason),
            None => eprintln!("stopped: {}", s.stop_reason),
        }
        Box::pin(async {})
    }
}

fn config_detail(cfg: &pagesift::config::Config) -> String {
    if cfg.config_files_disabled() {
        return "defaults (config files disabled by PAGESIFT_CONFIG=)".into();
    }
    if let Some(path) = cfg.source() {
        return path.display().to_string();
    }
    match pagesift::config::Config::default_path() {
        Some(path) => format!(
            "defaults (no config file; create {} to add one)",
            path.display()
        ),
        None => "defaults (no config file)".into(),
    }
}

/// 检查运行环境：浏览器、网络、数据目录。
pub async fn doctor(cfg: &pagesift::config::Config) -> Result<ExitCode> {
    let mut ok = true;
    let mut report = |good: bool, what: &str, detail: String| {
        println!("{} {what}: {detail}", if good { "ok  " } else { "FAIL" });
        ok &= good;
    };

    report(true, "config", config_detail(cfg));
    // LLM 和搜索是可选能力，没配置只提示，不算失败
    report(
        true,
        "llm",
        match &cfg.llm {
            Some(l) => format!("{} at {}", l.model, l.base_url),
            None => "not configured (optional): summary, json and extract are unavailable".into(),
        },
    );
    report(
        true,
        "search",
        match &cfg.search {
            Some(s) => s.backend.clone(),
            None => "not configured (optional): /v2/search is unavailable".into(),
        },
    );
    let mut sc = cfg.scraper(false)?;
    // 自检抓取不是真实流量：写进审计日志只会每次多一条可疑记录和快照
    sc.audit = None;
    let chain: Vec<String> =
        pagesift_cdp::engine_chain(&sc.browser, sc.fetcher.ssrf.allow_private_network)
            .iter()
            .filter_map(|c| match &c.source {
                pagesift_cdp::BrowserSource::Executable(p) => Some(format!(
                    "{} ({})",
                    c.engine.map_or("chrome", |k| k.name()),
                    p.display()
                )),
                pagesift_cdp::BrowserSource::Remote(u) => Some(format!("remote {u}")),
                pagesift_cdp::BrowserSource::Auto => None,
            })
            .collect();
    if chain.is_empty() {
        report(
            false,
            "browser",
            "no browser engine found; pages that need JavaScript will be returned from HTTP only. Install Lightpanda, moli or Chromium, or set the browser path".into(),
        );
    } else if !matches!(sc.browser.source, pagesift_cdp::BrowserSource::Remote(_))
        && pagesift_cdp::running_as_root()
    {
        report(
            false,
            "browser",
            format!(
                "{} installed, but rendering will fail: pagesift is running as root, and the Chrome sandbox does not work as root (pagesift will not disable it). Run as an unprivileged user",
                chain.join(" → ")
            ),
        );
    } else {
        report(true, "browser", chain.join(" → "));
    }

    let scraper = pagesift::Scraper::new(sc)?;
    let opts = pagesift::api::types::ScrapeOptions {
        engine: Some("http".into()),
        ..Default::default()
    };
    match scraper.scrape("https://example.com/", &opts).await {
        Ok(doc) => report(
            true,
            "network",
            format!(
                "fetched https://example.com/ (HTTP {})",
                doc.metadata.status_code
            ),
        ),
        Err(e) => report(
            false,
            "network",
            format!("{}: {}", e.code.as_str(), e.message),
        ),
    }

    let dir = cfg.data_dir();
    let writable = std::fs::create_dir_all(&dir).is_ok() && tempfile_in(&dir);
    report(writable, "data directory", dir.display().to_string());

    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn tempfile_in(dir: &std::path::Path) -> bool {
    let probe = dir.join(".pagesift-doctor");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}
