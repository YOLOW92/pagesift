//! CLI 辅助：crawl 的输出和 doctor 环境检查。

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use pagesift::api::types::{CrawlSummary, Document};
use pagesift::api::{ApiError, BoxFuture, CrawlSink};
use pagesift_sink::out::{DirOut, Fanout, LinesOut, PageOut, WarcOut};

/// crawl 的结果：有输出目录时每页一个 .md 文件，加一个 index.jsonl；否则每页一行 JSON 打到标准输出。
/// 给了 `--warc` 时同时写 WARC。布局在 `pagesift_sink::out`，和 scrape、convert 的 `--out` 一样。
pub struct CrawlOutput {
    dir: Option<Arc<DirOut>>,
    out: Fanout,
    counts: Mutex<(usize, usize)>,
    asset_hashes: Mutex<HashSet<String>>,
    /// 输出阶段的钩子（`SinkHook`）：写出前改写或丢弃文档。
    hooks: pagesift_hooks::Hooks,
}

impl CrawlOutput {
    pub fn new(dir: Option<PathBuf>, warc: Option<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.map(DirOut::create).transpose()?.map(Arc::new);
        let mut out = Fanout::default();
        match &dir {
            Some(d) => out.0.push(Box::new(Shared(d.clone()))),
            None => out.0.push(Box::new(LinesOut)),
        }
        if let Some(w) = warc {
            out.0.push(Box::new(WarcOut::create(&w)?));
        }
        Ok(Self {
            dir,
            out,
            counts: Mutex::new((0, 0)),
            asset_hashes: Mutex::new(HashSet::new()),
            hooks: Default::default(),
        })
    }

    pub fn sink_hooks(mut self, hooks: &pagesift_hooks::Hooks) -> Self {
        self.hooks = hooks.clone();
        self
    }

    pub fn counts(&self) -> (usize, usize) {
        *self.counts.lock().expect("lock")
    }

    fn write_page(&self, doc: &Document) -> std::io::Result<()> {
        self.counts.lock().expect("lock").0 += 1;
        if let Some(warnings) = doc.metadata.pagesift.as_ref().map(|p| &p.warnings) {
            for warning in warnings {
                if warning.starts_with("crawl_followed_zero:")
                    || warning.starts_with("crawl_scope:")
                {
                    eprintln!("warning: {warning}");
                }
            }
        }
        self.out.page(doc)
    }
}

/// 目录出口还要在结束时写 summary.json，所以 crawl 自己也持有一份。
struct Shared(Arc<DirOut>);

impl PageOut for Shared {
    fn page(&self, doc: &Document) -> std::io::Result<()> {
        self.0.page(doc)
    }

    fn graph_delta(&self, delta: &pagesift_graph::PageGraphDelta) -> std::io::Result<()> {
        self.0.graph_delta(delta)
    }

    fn graph_summary(&self, summary: &pagesift_graph::GraphSummary) -> std::io::Result<()> {
        self.0.graph_summary(summary)
    }
}

impl CrawlSink for CrawlOutput {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()> {
        let doc = if self.hooks.is_empty() {
            Some(doc)
        } else {
            self.hooks.run_sink(doc)
        };
        let Some(mut doc) = doc else {
            return Box::pin(async {});
        };
        // Serialize publication so a reference cannot precede its first bytes.
        let mut seen = self.asset_hashes.lock().unwrap_or_else(|e| e.into_inner());
        let mut published = HashSet::new();
        for a in &mut doc.attachments {
            if a.inline_data
                && a.data.is_some()
                && (seen.contains(&a.hash) || !published.insert(a.hash.clone()))
            {
                a.data = None;
            }
        }
        if let Err(e) = self.write_page(&doc) {
            eprintln!("could not write page: {e}");
        } else {
            seen.extend(published);
        }
        Box::pin(async {})
    }

    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()> {
        self.counts.lock().expect("lock").1 += 1;
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
        // 停止原因和覆盖度不只打在 stderr：--out 目录里另存一份，脚本按文件读
        // （stopReason、coverage（--adaptive 时）、termEvidence、pages，和 API 任务状态里的 pagesift 字段一样）
        if let Some(dir) = &self.dir {
            let written = serde_json::to_vec_pretty(&s)
                .map_err(std::io::Error::other)
                .and_then(|json| dir.summary(&json));
            if let Err(e) = written {
                eprintln!("could not write summary.json: {e}");
            }
        }
        if let Err(e) = self.out.finish() {
            eprintln!("could not finish output: {e}");
        }
        Box::pin(async {})
    }

    fn graph_delta(&self, delta: pagesift_graph::PageGraphDelta) -> BoxFuture<'_, ()> {
        if let Err(e) = self.out.graph_delta(&delta) {
            eprintln!("could not write graph delta: {e}");
        }
        Box::pin(async {})
    }

    fn graph_summary(&self, s: pagesift_graph::GraphSummary) -> BoxFuture<'_, ()> {
        eprintln!(
            "graph: {} nodes, {} citations, {} versions, {} fact clusters, {} conflicts",
            s.stats.nodes,
            s.stats.citations,
            s.stats.version_edges,
            s.stats.fact_clusters,
            s.stats.conflicts
        );
        if let Err(e) = self.out.graph_summary(&s) {
            eprintln!("could not write graph_summary.json: {e}");
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
    // 不影响退出码的问题：主力内核能用、兜底内核起不来
    let warn = |what: &str, detail: String| println!("WARN {what}: {detail}");

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
    report_ocr(cfg, &warn)?;
    let mut sc = cfg.scraper(false)?;
    // 自检抓取不是真实流量：写进审计日志只会每次多一条可疑记录和快照
    sc.audit = None;
    report(
        true,
        "browser fingerprint",
        match sc.fetcher.browser_fingerprint {
            Some(profile) => format!(
                "on ({profile}): HTTP fetches present TLS, HTTP/2 and headers of that browser, so sites see that browser and not pagesift; whether this meets a site's terms is up to the operator. robots.txt (as {:?}) and rate limits still apply",
                pagesift_net::product_token(&sc.fetcher.user_agent),
            ),
            None => format!(
                "off (default): HTTP fetches identify as {:?}",
                pagesift_net::product_token(&sc.fetcher.user_agent),
            ),
        },
    );
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
    }
    let probe = !chain.is_empty()
        && (matches!(sc.browser.source, pagesift_cdp::BrowserSource::Remote(_))
            || !pagesift_cdp::running_as_root());
    if probe {
        report(true, "browser", chain.join(" → "));
    }

    let scraper = pagesift::Scraper::new(sc)?;
    // 文件在不等于能启动：逐个真的启动一次。一个都起不来算失败；只是兜底的起不来给警告
    if probe {
        let probes = scraper.probe_engines().await;
        let started = probes.iter().filter(|p| p.result.is_ok()).count();
        let first_ok = probes.iter().position(|p| p.result.is_ok());
        for (i, p) in probes.iter().enumerate() {
            match &p.result {
                Ok(()) => report(
                    true,
                    &format!("engine {}", p.engine),
                    format!("starts ({})", p.location),
                ),
                Err(e) if started == 0 => report(
                    false,
                    &format!("engine {}", p.engine),
                    format!("does not start ({}): {e}", p.location),
                ),
                Err(e) => {
                    // 排在第一个能用的内核前面：页面改由它渲染；排在后面：它兜不了底
                    let effect = match first_ok {
                        Some(j) if j > i => {
                            format!("pages are rendered by {} instead", probes[j].engine)
                        }
                        _ => "pages the engines before it cannot render get no fallback".into(),
                    };
                    warn(
                        &format!("engine {}", p.engine),
                        format!("does not start ({}): {e}; {effect}", p.location),
                    )
                }
            }
        }
        scraper.shutdown().await;
    }
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

/// doctor 的 OCR 和 GPU 部分：没打开 OCR、没有显卡都不算失败，只如实说明。
fn report_ocr(cfg: &pagesift::config::Config, warn: &impl Fn(&str, String)) -> Result<()> {
    use pagesift_ocr::device::Device;
    const MIB: u64 = 1 << 20; // [协议] 1 MiB
    let device = match cfg.ocr_device() {
        Ok((device, _)) => device,
        Err(e) => {
            warn("gpu", format!("{e:#}"));
            return Ok(());
        }
    };
    let next = match device {
        Device::Cpu => {
            println!("ok   gpu: off");
            "cpu".to_string()
        }
        Device::Gpu { index } => match pagesift_gpu::probe(index) {
            Ok(g) => {
                let (major, minor) = g.compute_capability;
                println!(
                    "ok   gpu: {} (compute capability {major}.{minor}, {} of {} MiB free, driver {}, kernels {})",
                    g.name,
                    g.memory_free / MIB,
                    g.memory_total / MIB,
                    g.driver_version.as_deref().unwrap_or("unknown"),
                    g.ptx,
                );
                format!("cuda:{index}")
            }
            Err(e) => {
                println!("ok   gpu: not available: {e}");
                "cpu".to_string()
            }
        },
    };
    // 没打开 OCR 时不看模型：HOME 没设之类的问题不该让 doctor 中止
    if !cfg.ocr.enabled {
        println!(
            "ok   ocr: off (optional): scanned PDF pages have no text. Turn on with [ocr] enabled = true or --ocr"
        );
        return Ok(());
    }
    let root = cfg.models_root()?;
    let choice = cfg.ocr_models()?;
    let math: &[pagesift_ocr::models::Model] = if cfg.ocr.formulas {
        &[pagesift_ocr::models::LAYOUT, pagesift_ocr::models::FORMULA]
    } else {
        &[]
    };
    let missing: Vec<&str> = [&choice.det, &choice.rec]
        .into_iter()
        .chain(math)
        .filter(|m| !m.present(&root))
        .map(|m| m.repo)
        .collect();
    let where_ = root.display();
    if missing.is_empty() {
        println!(
            "ok   ocr: {} and {} in {where_}; the next inference runs on {next}",
            choice.det.repo, choice.rec.repo
        );
    } else {
        warn(
            "ocr",
            format!(
                "{} not in {where_}; downloaded the first time a scanned page needs them, or now with `pagesift models fetch`; the next inference runs on {next}",
                missing.join(", ")
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod image_asset_tests {
    use super::*;

    #[tokio::test]
    async fn crawl_directory_reuses_one_asset_file() {
        let dir = tempfile::tempdir().unwrap();
        let sink = CrawlOutput::new(Some(dir.path().to_owned()), None).unwrap();
        let mut a = pagesift::api::types::Attachment::new(
            b"image".to_vec(),
            "png",
            Some("https://example.com/a.png".into()),
        );
        a.inline_data = true;
        let id = a.id.clone();
        for n in 0..2 {
            let mut doc = Document {
                markdown: Some(format!("![x](asset://{id})")),
                attachments: vec![a.clone()],
                ..Default::default()
            };
            doc.metadata.source_url = Some(format!("https://example.com/{n}"));
            sink.page(doc).await;
        }
        assert_eq!(
            std::fs::read_dir(dir.path().join("images"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read(dir.path().join("images").join(id)).unwrap(),
            b"image"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("index.jsonl"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }
}
