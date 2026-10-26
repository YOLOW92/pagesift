//! 页面抓取、转换、爬取与基准类子命令。

use crate::*;

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
        } => {
            let (html, had_errors, encoding) = read_input(file, charset.as_deref())?;
            if had_errors {
                eprintln!(
                    "warning: input could not be fully decoded as {encoding}; output may contain replacement characters (try --charset)"
                );
            }
            let doc = process(
                &html,
                &Options {
                    base_url,
                    only_main_content: !full,
                    ..Default::default()
                },
            );
            print!("{}", doc.markdown);
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
            allow,
        } => runtime()?.block_on(async move {
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
                ..Default::default()
            };
            if format == "json" {
                opts.formats
                    .push(pagesift::api::types::Format::new("links"));
            }
            // 本地文件（路径或 file:// 网址）按上传解析：PDF、HTML、Word、Excel、PowerPoint、EPUB、CSV
            let result = match local_file(&url) {
                Some(path) => parse_local(&scraper, &path, &opts).await?,
                None => scraper.scrape(&url, &opts).await,
            };
            scraper.shutdown().await;
            match result {
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
            depth,
            include,
            exclude,
            entire_domain,
            scope,
            subdomains,
            sitemap,
            engine,
            external_scripts,
            query,
            adaptive,
            tuning_mode,
            allow,
        } => runtime()?.block_on(async move {
            let req: pagesift::api::types::CrawlRequest =
                serde_json::from_value(serde_json::json!({
                    "url": url,
                    "limit": limit,
                    "maxDiscoveryDepth": depth,
                    "includePaths": include,
                    "excludePaths": exclude,
                    "crawlEntireDomain": entire_domain,
                    "scope": scope,
                    "allowSubdomains": subdomains,
                    "sitemap": sitemap,
                    "scrapeOptions": { "engine": engine, "discoverScripts": external_scripts },
                    "query": query,
                    "adaptive": adaptive,
                }))?;
            if let Some(dir) = &out {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            let mut scraper_config = cfg.scraper(false)?;
            allow.apply(&mut scraper_config);
            let tuning_mode = tuning_mode
                .parse::<pagesift::adaptive::TuningMode>()
                .map_err(anyhow::Error::msg)?;
            pagesift::adaptive::TuningProfile::new(tuning_mode)
                .apply_to_config(&mut scraper_config);
            let scraper = pagesift::Scraper::new(scraper_config)?;
            let sink = Arc::new(cli::CrawlOutput::new(out));
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
            allow,
        } => runtime()?.block_on(async move {
            let req: pagesift::api::types::MapRequest =
                serde_json::from_value(serde_json::json!({
                    "url": url, "limit": limit, "search": search, "sitemap": sitemap,
                }))?;
            let mut scraper_config = cfg.scraper(false)?;
            allow.apply(&mut scraper_config);
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
    };
    Ok(scraper.parse(upload, opts).await)
}
