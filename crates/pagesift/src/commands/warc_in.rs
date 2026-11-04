//! 从 WARC 读页面（不联网）：`pagesift scrape x.warc[.gz]` 和 `pagesift crawl --from-warc x.warc URL`。
//!
//! 读 `response`（HTTP 报文，由 `pagesift_net::http_message` 解析：分块传输、Content-Encoding）和
//! `resource`（pagesift 自己导出的就是这种）记录；同一网址只取第一条可转换的记录。只转换 2xx 的响应；
//! 其余记录类型（warcinfo、request、metadata、conversion、revisit）跳过。每一页按上传文件解析
//! （HTML、PDF、Office），`metadata.sourceURL` 是记录的 `WARC-Target-URI`。

use std::collections::HashSet;
use std::path::Path;

use pagesift::api::types::{Document, ScrapeOptions, Upload, UploadKind};
use pagesift::api::{ApiError, MAX_PARSE_FILE_BYTES};

use crate::*;

/// 文件名像 WARC（`.warc`、`.warc.gz`）。
pub(crate) fn is_warc(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name.ends_with(".warc") || name.ends_with(".warc.gz")
}

/// 存档里的一页：网址和可以交给 `Scraper::parse` 的文件。
pub(crate) struct ArchivedPage {
    pub url: String,
    pub upload: Upload,
}

/// 逐条读出可转换的页面，交给 `each`；返回跳过的记录数（非 2xx、类型认不出、块过大）。
pub(crate) fn for_each_page(path: &Path, mut each: impl FnMut(ArchivedPage)) -> Result<usize> {
    let reader = pagesift_store::warc::reader::open(path, MAX_PARSE_FILE_BYTES as u64)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut seen = HashSet::new();
    let mut skipped = 0;
    for rec in reader {
        let rec = rec.with_context(|| format!("reading {}", path.display()))?;
        let kind = rec.record_type();
        if kind != "response" && kind != "resource" {
            continue;
        }
        let Some(url) = rec.target_uri().map(str::to_string) else {
            skipped += 1;
            continue;
        };
        if rec.truncated || seen.contains(&url) {
            skipped += 1;
            continue;
        }
        let (bytes, content_type) = if kind == "response" {
            match pagesift_net::http_message::parse_response(&rec.block, MAX_PARSE_FILE_BYTES) {
                // [协议] 2xx 成功响应（RFC 9110 §15.3）
                Ok(m) if (200..300).contains(&m.status) => {
                    let ct = m
                        .headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    (m.body, ct)
                }
                _ => {
                    skipped += 1;
                    continue;
                }
            }
        } else {
            (rec.block.clone(), rec.content_type().map(str::to_string))
        };
        let filename = url::Url::parse(&url)
            .ok()
            .and_then(|u| {
                u.path_segments()
                    .and_then(|mut s| s.next_back().map(str::to_string))
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "index.html".into());
        // 存档和抓取一个口径：文本类和图片不转（存档里的 robots.txt、接口 JSON、图片都不算页面）
        let Some(kind) = UploadKind::detect(&bytes, &filename, content_type.as_deref())
            .filter(|k| k.fetchable())
        else {
            skipped += 1;
            continue;
        };
        seen.insert(url.clone());
        let upload = Upload {
            bytes,
            filename,
            content_type,
            kind,
            url: url::Url::parse(&url).ok(),
        };
        each(ArchivedPage { url, upload });
    }
    Ok(skipped)
}

/// `scrape x.warc`：转换存档里的每一页。
pub(crate) async fn scrape_archive(
    scraper: &pagesift::Scraper,
    path: &Path,
    opts: &ScrapeOptions,
) -> Result<Vec<(String, Result<Document, ApiError>)>> {
    let mut pages = Vec::new();
    let skipped = for_each_page(path, |p| pages.push(p))?;
    if skipped > 0 {
        eprintln!("warning: archive_records_skipped: {skipped} records were not convertible pages");
    }
    let mut out = Vec::with_capacity(pages.len());
    for p in pages {
        let doc = scraper.parse(p.upload, opts).await;
        out.push((p.url, doc));
    }
    Ok(out)
}

/// 打印或写出存档转换的结果：一页时和普通 scrape 一样；多页时 Markdown 每页前加一行
/// `<!-- pagesift: <url> -->`，`--format json` 每页一行 JSON。有页面失败时退出码为 1。
pub(crate) fn emit(
    pages: Vec<(String, Result<Document, ApiError>)>,
    format: &str,
    out: Option<&Path>,
) -> Result<ExitCode> {
    use pagesift_sink::out::PageOut;
    let dir = match out {
        Some(d) => Some(pagesift_sink::out::DirOut::create(d)?),
        None => None,
    };
    let many = pages.len() > 1;
    let mut failed = false;
    for (url, r) in pages {
        match r {
            Ok(doc) => {
                print_scrape_warnings(&doc);
                if let Some(d) = &dir {
                    d.page(&doc)?;
                } else if format == "json" {
                    println!("{}", serde_json::json!({ "success": true, "data": doc }));
                } else {
                    if many {
                        println!("<!-- pagesift: {url} -->");
                    }
                    print!("{}", doc.markdown.unwrap_or_default());
                    if many {
                        println!();
                    }
                }
            }
            Err(e) => {
                failed = true;
                eprintln!("{url}: {}: {}", e.code.as_str(), e.message);
            }
        }
    }
    if let Some(d) = &dir {
        d.finish()?;
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// `crawl --from-warc`：存档里和起始网址同主机（`--subdomains` 时同站点）、路径通过
/// `--include`/`--exclude` 的页面，按存档顺序最多 `limit` 页；不跟链接、不联网。
pub(crate) async fn crawl_archive(
    scraper: &pagesift::Scraper,
    path: &Path,
    seed: &str,
    req: &pagesift::api::types::CrawlRequest,
    sink: Arc<cli::CrawlOutput>,
) -> Result<()> {
    use pagesift::api::CrawlSink;
    let seed = url::Url::parse(seed).context("the start URL")?;
    let host = seed.host_str().unwrap_or_default().to_string();
    let site = pagesift_content::site_key(&host);
    let any_sub = req.allow_subdomains.unwrap_or(false);
    let include: Vec<regex::Regex> = req
        .include_paths
        .iter()
        .map(|p| regex::Regex::new(p))
        .collect::<std::result::Result<_, _>>()?;
    let exclude: Vec<regex::Regex> = req
        .exclude_paths
        .iter()
        .map(|p| regex::Regex::new(p))
        .collect::<std::result::Result<_, _>>()?;
    let mut pages = Vec::new();
    let skipped = for_each_page(path, |p| {
        let Ok(u) = url::Url::parse(&p.url) else {
            return;
        };
        let h = u.host_str().unwrap_or_default();
        let in_host = h == host || (any_sub && pagesift_content::site_key(h) == site);
        let path = u.path();
        let keep = in_host
            && (include.is_empty() || include.iter().any(|r| r.is_match(path)))
            && !exclude.iter().any(|r| r.is_match(path));
        if keep && pages.len() < req.limit {
            pages.push(p);
        }
    })?;
    if skipped > 0 {
        eprintln!("warning: archive_records_skipped: {skipped} records were not convertible pages");
    }
    let n = pages.len();
    sink.total(n).await;
    for p in pages {
        match scraper.parse(p.upload, &req.scrape_options).await {
            Ok(doc) => sink.page(doc).await,
            Err(e) => sink.error(&p.url, e).await,
        }
    }
    // 结束输出（写 summary.json、刷 WARC）：存档里在范围内的页面都转换完了
    sink.summary(pagesift::api::types::CrawlSummary {
        stop_reason: if n >= req.limit {
            "limit_reached"
        } else {
            "exhausted"
        }
        .into(),
        coverage: None,
        term_evidence: Default::default(),
        pages: n,
    })
    .await;
    Ok(())
}
