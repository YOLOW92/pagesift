//! 页面抓取、转换、爬取与基准类子命令。

use crate::*;

/// `crawl` 没给 `--limit` 时的页数上限。
const CRAWL_DEFAULT_LIMIT: usize = 100;
/// `crawl --adaptive` 没给 `--limit` 时的上限：页数由自适应停止决定，这只是防失控的保险。
const CRAWL_ADAPTIVE_CAP: usize = 500;

pub(crate) fn run(command: Command, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match command {
        Command::Collect {
            url,
            source,
            data_dir,
            adapter,
            authoritative,
            max_pages,
            out,
            restart,
            allow,
        } => {
            use pagesift::collect::{Adapter, Collection, EventLog, JsonAdapter};
            let adapter = match adapter {
                Some(path) => Adapter::Json(serde_json::from_slice::<JsonAdapter>(
                    &std::fs::read(path)?,
                )?),
                None => {
                    anyhow::ensure!(
                        url.path().contains("/wp-json/wp/v2/"),
                        "provide --adapter for a non-WordPress API"
                    );
                    Adapter::WordPress
                }
            };
            let store = pagesift_store::Store::open_dir(data_dir)?;
            let mut collection = Collection::open(&store, &source, adapter)?;
            if restart {
                collection.abort()?;
            }
            collection.begin(&url, authoritative)?;
            let mut sc = cfg.scraper(false)?;
            allow.apply(&mut sc);
            let fetcher = Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?);
            let policy = pagesift_policy::Policy::new(fetcher, sc.policy);
            let mut sink = EventLog::open(out)?;
            let complete =
                runtime()?.block_on(
                    collection.run(&policy, max_pages, &mut |event| sink.append(event)),
                )?;
            eprintln!(
                "collection {}",
                if complete {
                    "complete"
                } else {
                    "checkpoint saved; resume with the same command"
                }
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Convert {
            file,
            base_url,
            full,
            charset,
            out,
            export_images,
        } => {
            let (html, had_errors, encoding) = read_input(file, charset.as_deref())?;
            if export_images {
                return convert_with_images(&cfg, html, base_url, full, out);
            }
            if had_errors {
                eprintln!(
                    "warning: input could not be fully decoded as {encoding}; output may contain replacement characters (try --charset)"
                );
            }
            let doc = process(
                &html,
                &Options {
                    base_url: base_url.clone(),
                    only_main_content: !full,
                    ..Default::default()
                },
            );
            match out {
                Some(dir) => {
                    use pagesift_sink::out::PageOut;
                    let mut meta = pagesift::api::types::Metadata {
                        source_url: base_url.map(|u| u.to_string()),
                        ..Default::default()
                    };
                    pagesift_sink::meta::fill(&mut meta, &doc.metadata);
                    let page = pagesift::api::types::Document {
                        markdown: Some(doc.markdown),
                        metadata: meta,
                        ..Default::default()
                    };
                    pagesift_sink::out::DirOut::create(dir)?.page(&page)?;
                }
                None => print!("{}", doc.markdown),
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Bench {
            dir,
            out,
            runs,
            limit,
            auto,
            emit_inputs,
            external,
            jobs,
        } => {
            bench::run(bench::Args {
                dir,
                out,
                runs,
                limit,
                auto,
                emit_inputs,
                external,
                jobs,
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::BenchRender {
            dir,
            limit,
            concurrency,
        } => {
            bench::render_snapshots(&dir, limit, concurrency)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Scrape {
            url,
            format,
            engine,
            full,
            ignore_robots,
            proxy,
            out,
            warc,
            explain,
            export_images,
            allow,
        } => light_runtime()?.block_on(async move {
            let mut sc = cfg.scraper(false)?;
            if let Some(p) = proxy {
                sc.fetcher.proxy = Some(p);
            }
            allow.apply(&mut sc);
            let scraper = pagesift::Scraper::new(sc)?;
            let mut opts = pagesift::api::types::ScrapeOptions {
                only_main_content: !full,
                ignore_robots_txt: ignore_robots,
                engine: Some(engine),
                explain,
                export_images,
                ..Default::default()
            };
            if format == "json" {
                opts.formats
                    .push(pagesift::api::types::Format::new("links"));
            }
            // WARC 要页面 HTML：内部多要一份 rawHtml，写完 WARC 再去掉
            if warc.is_some() {
                opts.formats
                    .push(pagesift::api::types::Format::new("rawHtml"));
            }
            // WARC 存档：逐页转换，不联网
            if let Some(path) = local_file(&url).filter(|p| super::warc_in::is_warc(p)) {
                let pages = super::warc_in::scrape_archive(&scraper, &path, &opts).await;
                scraper.shutdown().await;
                return super::warc_in::emit(pages?, &format, out.as_deref());
            }
            // 本地文件（路径或 file:// 网址）按上传解析：PDF、HTML、Word、Excel、PowerPoint、EPUB、CSV
            let result = match local_file(&url) {
                Some(path) => parse_local(&scraper, &path, &opts).await?,
                None => scraper.scrape(&url, &opts).await,
            };
            scraper.shutdown().await;
            let result = result.and_then(|mut doc| {
                use pagesift_sink::out::PageOut;
                let written = (|| -> std::io::Result<()> {
                    if let Some(path) = &warc {
                        let w = pagesift_sink::out::WarcOut::create(path)?;
                        w.page(&doc)?;
                        w.finish()?;
                        doc.raw_html = None;
                    }
                    if let Some(dir) = &out {
                        pagesift_sink::out::DirOut::create(dir)?.page(&doc)?;
                    }
                    Ok(())
                })();
                written.map(|()| doc).map_err(|e| {
                    pagesift::api::ApiError::new(pagesift::api::ErrorCode::Internal, e.to_string())
                })
            });
            if let Ok(doc) = &result
                && let Some(plan) = doc.metadata.pagesift.as_ref().and_then(|m| m.plan.as_ref())
            {
                eprintln!("plan: {}", serde_json::to_string(plan)?);
            }
            match result {
                Ok(doc) if out.is_some() => {
                    print_scrape_warnings(&doc);
                    Ok(scrape_exit_code(&doc))
                }
                Ok(doc) if format == "json" => {
                    let code = scrape_exit_code(&doc);
                    print_scrape_warnings(&doc);
                    let body = serde_json::json!({ "success": true, "data": doc });
                    println!("{}", serde_json::to_string_pretty(&body)?);
                    Ok(code)
                }
                Ok(doc) => {
                    let code = scrape_exit_code(&doc);
                    print_scrape_warnings(&doc);
                    print!("{}", doc.markdown.unwrap_or_default());
                    Ok(code)
                }
                Err(e) => {
                    eprintln!("{}: {}", e.code.as_str(), e.message);
                    if let Some(d) = &e.details {
                        eprintln!("{d}");
                    }
                    Ok(ExitCode::from(1))
                }
            }
        }),
        Command::Crawl {
            url,
            limit,
            out,
            warc,
            depth,
            include,
            exclude,
            entire_domain,
            scope,
            subdomains,
            sitemap,
            engine,
            ignore_robots,
            external_scripts,
            query,
            adaptive,
            attachments,
            attachment_limit,
            tuning_mode,
            export_images,
            from_warc,
            allow,
        } => runtime()?.block_on(async move {
            // 自适应时页数由停止规则决定，没给 --limit 时只留一个防失控的上限
            let limit = limit.unwrap_or(if adaptive {
                CRAWL_ADAPTIVE_CAP
            } else {
                CRAWL_DEFAULT_LIMIT
            });
            let mut req: pagesift::api::types::CrawlRequest =
                serde_json::from_value(serde_json::json!({
                    "url": url,
                    "limit": limit,
                    "maxDiscoveryDepth": depth,
                    "includePaths": include,
                    "excludePaths": exclude,
                    "crawlEntireDomain": entire_domain,
                    "scope": scope,
                    "allowSubdomains": subdomains.then_some(true),
                    "sitemap": sitemap,
                    "scrapeOptions": {
                        "engine": engine,
                        "discoverScripts": external_scripts,
                        "exportImages": export_images,
                    },
                    "query": query,
                    "adaptive": adaptive,
                    "attachments": attachments,
                    "attachmentLimit": attachment_limit,
                    "ignoreRobotsTxt": ignore_robots,
                }))?;
            let mut scraper_config = cfg.scraper(false)?;
            allow.apply(&mut scraper_config);
            // 跑命令行的人就是管理员：--ignore-robots 本身就是许可（HTTP API 仍要配置里放行）
            scraper_config.policy.allow_crawl_ignore_robots |= ignore_robots;
            // 爬取命令的超时先验沿用 0.81：TuningProfile 删除后若退回通用默认（连接 10 s、总计 60 s），
            // 死站点会把爬取拖慢。用户写了超时（`[fetch] timeout_secs`）时不动。
            if !scraper_config.fetcher.timeouts_explicit {
                let f = &mut scraper_config.fetcher;
                // [策略] 先验包 v0：0.81 crawl 命令（TuningProfile 中档、高档机器）的现值，连接类 3.5 s
                let connect = std::time::Duration::from_millis(3500);
                // [策略] 先验包 v0：0.81 crawl 命令的现值，首字节、读正文、总计 10 s
                let total = std::time::Duration::from_secs(10);
                f.connect_timeout = connect;
                f.dns_timeout = connect;
                f.tls_timeout = connect;
                f.first_byte_timeout = total;
                f.body_idle_timeout = total;
                f.total_timeout = total;
            }
            // 全局在途数的冷启动值也沿用 0.81 crawl 命令的值（之后由 USL 在线调整）；
            // 退回通用的 64 会让多站点爬取起步变慢。配置里写了 `global_concurrency` 时不动。
            if scraper_config.policy.rate.global_ceiling.is_none() {
                scraper_config.policy.rate.global_concurrency = crawl_concurrency_prior();
            }
            if tuning_mode.is_some() {
                eprintln!(
                    "note: --tuning-mode is ignored; concurrency adapts online (USL, latency gradient)"
                );
            }
            let scraper = pagesift::Scraper::new(scraper_config)?;
            if warc.is_some() {
                req.scrape_options
                    .formats
                    .push(pagesift::api::types::Format::new("rawHtml"));
            }
            let sink = Arc::new(
                cli::CrawlOutput::new(out, warc)
                    .context("opening the output")?
                    .sink_hooks(scraper.hooks()),
            );
            if let Some(path) = from_warc {
                let r = super::warc_in::crawl_archive(&scraper, &path, &url, &req, sink.clone()).await;
                scraper.shutdown().await;
                r?;
                let (pages, errors) = sink.counts();
                eprintln!("converted {pages} archived pages, {errors} errors");
                return Ok(if pages == 0 && errors > 0 {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                });
            }
            let result = scraper.crawl(&req, sink.clone()).await;
            scraper.shutdown().await;
            let (pages, errors) = sink.counts();
            eprintln!("crawled {pages} pages, {errors} errors");
            match result {
                // 一页都没抓到且至少有一个错误：种子就失败了，不能报成功。
                Ok(()) if pages == 0 && errors > 0 => Ok(ExitCode::from(1)),
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => {
                    eprintln!("{}: {}", e.code.as_str(), e.message);
                    Ok(ExitCode::from(1))
                }
            }
        }),
        Command::Map {
            url,
            limit,
            search,
            sitemap,
            ignore_robots,
            allow,
        } => runtime()?.block_on(async move {
            let req: pagesift::api::types::MapRequest =
                serde_json::from_value(serde_json::json!({
                    "url": url, "limit": limit, "search": search, "sitemap": sitemap,
                    "ignoreRobotsTxt": ignore_robots,
                }))?;
            let mut scraper_config = cfg.scraper(false)?;
            allow.apply(&mut scraper_config);
            scraper_config.policy.allow_crawl_ignore_robots |= ignore_robots;
            let scraper = pagesift::Scraper::new(scraper_config)?;
            match scraper.map(&req).await {
                Ok(links) => {
                    for l in links {
                        println!("{}", l.url);
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!("{}: {}", e.code.as_str(), e.message);
                    Ok(ExitCode::from(1))
                }
            }
        }),
        _ => unreachable!(),
    }
}

/// `scrape` 的参数是本地文件时的路径：`file://` 网址，或者没有协议、确实存在的路径。
fn local_file(arg: &str) -> Option<std::path::PathBuf> {
    if arg.starts_with("file://") {
        return url::Url::parse(arg).ok()?.to_file_path().ok();
    }
    let path = std::path::Path::new(arg);
    (!arg.contains("://") && path.is_file()).then(|| path.to_path_buf())
}

/// `convert --export-images`：本地 HTML 按上传解析（`--base-url` 作文档地址），图片经礼貌抓取路径取回，
/// 和 scrape 的 `--out` 同一种布局。
fn convert_with_images(
    cfg: &pagesift::config::Config,
    html: String,
    base_url: Option<Url>,
    full: bool,
    out: Option<PathBuf>,
) -> Result<ExitCode> {
    use pagesift::api::types::{Upload, UploadKind};
    use pagesift_sink::out::PageOut;
    let dir = out.context("--export-images needs --out")?;
    light_runtime()?.block_on(async move {
        let scraper = pagesift::Scraper::new(cfg.scraper(false)?)?;
        let opts = pagesift::api::types::ScrapeOptions {
            only_main_content: !full,
            export_images: true,
            ..Default::default()
        };
        let upload = Upload {
            bytes: html.into_bytes(),
            filename: "input.html".into(),
            content_type: Some("text/html; charset=utf-8".into()),
            kind: UploadKind::Html,
            url: base_url,
        };
        let result = scraper.parse(upload, &opts).await;
        scraper.shutdown().await;
        let doc = result.map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
        print_scrape_warnings(&doc);
        pagesift_sink::out::DirOut::create(dir)?.page(&doc)?;
        Ok(ExitCode::SUCCESS)
    })
}

async fn parse_local(
    scraper: &pagesift::Scraper,
    path: &std::path::Path,
    opts: &pagesift::api::types::ScrapeOptions,
) -> Result<Result<pagesift::api::types::Document, pagesift::api::ApiError>> {
    use pagesift::api::types::{Upload, UploadKind};
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let filename = path
        .file_name()
        .map_or_else(|| "upload".into(), |n| n.to_string_lossy().into_owned());
    let Some(kind) = UploadKind::detect(&bytes, &filename, None) else {
        return Ok(Err(pagesift::api::ApiError::unsupported(format!(
            "unsupported file type: {filename}"
        ))));
    };
    let upload = Upload {
        bytes,
        filename,
        content_type: None,
        kind,
        url: None,
    };
    Ok(scraper.parse(upload, opts).await)
}

/// 爬取命令全局在途数的冷启动值：0.81 `CpuTopology` 的初值公式（每个逻辑 CPU 16 个、或每个本地
/// 物理核 24 个，取大），再受内存（每个在途请求 2 MiB）和文件描述符（软上限的 70%）限制。
fn crawl_concurrency_prior() -> usize {
    // [策略] 先验包 v0：0.81 的现值（`adaptive/telemetry.rs` 的 cpu_base、mem_cap、fd_cap）
    const PER_CPU: usize = 16;
    // [策略] 先验包 v0：同上
    const PER_LOCAL_PHYSICAL_CORE: usize = 24;
    // [策略] 先验包 v0：同上，每个在途请求 2 MiB
    const BYTES_PER_REQUEST: u64 = 2 << 20;
    let topology = pagesift_policy::topology::CpuTopology::detect();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let cpu = (cores * PER_CPU).max(topology.physical_cores_per_numa() * PER_LOCAL_PHYSICAL_CORE);
    let mem = pagesift_net::resources::read_memory()
        .map_or(usize::MAX, |m| (m.total_bytes / BYTES_PER_REQUEST) as usize);
    // [策略] 先验包 v0：同上，留 30% 描述符给 DNS、日志
    let fd = pagesift_net::resources::fd_soft_limit().map_or(usize::MAX, |n| n * 7 / 10);
    // [策略] 先验包 v0：同上，下限 4
    cpu.min(mem).min(fd).max(4)
}
