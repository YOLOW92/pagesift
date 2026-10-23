#![forbid(unsafe_code)]

mod bench;
mod cli;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pagesift_content::{Options, process};
use tracing_subscriber::EnvFilter;
use url::Url;

#[derive(Parser)]
#[command(
    name = "pagesift",
    version,
    about = "Turn web pages into clean Markdown for LLMs"
)]
struct Cli {
    /// Config file (default: $PAGESIFT_CONFIG); see pagesift.example.toml
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect local form metadata and redacted technology/secret indicators
    Inspect {
        file: Option<PathBuf>,
        #[arg(long)]
        base_url: Url,
    },
    /// Find a likely element after layout changes, with explicit abstention
    Relocate {
        file: PathBuf,
        #[arg(long)]
        original: PathBuf,
        #[arg(long)]
        selector: String,
    },
    /// Discover same-origin external JS modules and static navigation candidates
    DiscoverJs {
        url: Url,
        #[arg(long, default_value_t = 32)]
        max_files: usize,
        #[arg(long, default_value_t = 3)]
        max_depth: usize,
    },
    /// Extract original media URLs with an explicit site/protocol adapter
    MediaExtract {
        file: Option<PathBuf>,
        #[arg(long)]
        base_url: Url,
        #[arg(long)]
        adapter: pagesift::media::MediaAdapter,
    },
    /// Archive a media API response, then publish named files and metadata
    MediaArchive {
        url: Url,
        #[arg(long)]
        adapter: pagesift::media::MediaAdapter,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "{id}-{hash}.{ext}")]
        template: String,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: u64,
    },
    /// Plan finite unencrypted HLS/DASH without downloading segments
    StreamPlan {
        file: Option<PathBuf>,
        #[arg(long)]
        base_url: Url,
    },
    /// Archive finite HLS/DASH locally (no A/V remux)
    StreamDownload {
        url: Url,
        #[arg(long)]
        out: PathBuf,
        /// Select this zero-based HLS master variant; no automatic selection
        #[arg(long)]
        variant: Option<usize>,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: u64,
    },
    /// Convert a local HTML file (or stdin) to Markdown
    Convert {
        /// HTML file to read; reads stdin when omitted or "-"
        file: Option<PathBuf>,
        /// URL the HTML was fetched from, used to make links absolute
        #[arg(long)]
        base_url: Option<Url>,
        /// Keep the whole page instead of only the main content
        #[arg(long)]
        full: bool,
    },
    /// Incrementally collect a JSON API; emit acknowledged upsert/delete events
    Collect {
        url: Url,
        #[arg(long)]
        source: String,
        #[arg(long)]
        data_dir: PathBuf,
        /// Durable append-only event log (replays may contain duplicate events)
        #[arg(long)]
        out: PathBuf,
        /// Abandon an incomplete scan and restart; committed inventory is retained
        #[arg(long)]
        restart: bool,
        /// Adapter JSON file; omitted for a WordPress REST collection
        #[arg(long)]
        adapter: Option<PathBuf>,
        /// Declare the listing complete and authoritative for deletion sync
        #[arg(long)]
        authoritative: bool,
        #[arg(long, default_value_t = 100)]
        max_pages: usize,
    },
    /// Archive one direct media URL with validated resume and a stable identity
    Download {
        url: Url,
        #[arg(long)]
        source: String,
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "1")]
        revision: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 268_435_456)]
        max_bytes: u64,
        #[arg(long)]
        expected_blake3: Option<String>,
        /// Discard an incomplete transfer; completed items remain immutable
        #[arg(long)]
        restart: bool,
    },
    /// Fetch one HTTP page with an explicit persistent, origin-isolated cookie session
    SessionFetch {
        url: Url,
        #[arg(long)]
        state: PathBuf,
        /// Explicitly replace stored cookies and retired/expired session state
        #[arg(long)]
        reset_session: bool,
        #[arg(long, default_value = "md", value_parser = ["md", "json"])]
        format: String,
        #[arg(long)]
        full: bool,
    },
    /// Start the Firecrawl-compatible HTTP API
    Serve {
        /// Port (default: server.port in the config, else 3002)
        #[arg(long)]
        port: Option<u16>,
        /// Address to listen on (default: server.host in the config, else 127.0.0.1)
        #[arg(long)]
        host: Option<String>,
        /// Require `Authorization: Bearer <key>` (repeatable); also read from PAGESIFT_API_KEYS (comma separated)
        #[arg(long = "api-key")]
        api_keys: Vec<String>,
        /// Public base URL used in job links (default: from the Host header)
        #[arg(long)]
        public_url: Option<String>,
        /// Where the cache lives (default: $PAGESIFT_DATA_DIR, else ~/.local/share/pagesift)
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Disable the result cache
        #[arg(long)]
        no_cache: bool,
    },
    /// Scrape a single URL
    Scrape {
        url: String,
        /// Output: md prints the Markdown, json prints the Firecrawl response
        #[arg(long, default_value = "md")]
        format: String,
        /// http, browser or auto
        #[arg(long, default_value = "auto")]
        engine: String,
        /// Keep the whole page instead of only the main content
        #[arg(long)]
        full: bool,
        /// Ignore robots.txt for this page (single-page scrapes only)
        #[arg(long)]
        ignore_robots: bool,
        /// Egress proxy URL (e.g. http://127.0.0.1:7890, socks5://127.0.0.1:1080)
        #[arg(long)]
        proxy: Option<String>,
        /// Allow private CIDR subnets (e.g. 10.0.0.0/8, 198.18.0.0/15)
        #[arg(long = "allow-cidr")]
        allow_cidrs: Vec<String>,
    },
    /// Crawl a site; writes one Markdown file per page with --out, else JSON lines to stdout
    Crawl {
        url: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Directory for the Markdown files and an index.jsonl
        #[arg(long)]
        out: Option<PathBuf>,
        /// Maximum discovery depth from the start page
        #[arg(long)]
        depth: Option<usize>,
        /// Only follow paths matching this regex (repeatable)
        #[arg(long)]
        include: Vec<String>,
        /// Skip paths matching this regex (repeatable)
        #[arg(long)]
        exclude: Vec<String>,
        /// Crawl the whole domain, not just pages under the start path
        #[arg(long)]
        entire_domain: bool,
        /// Follow links to subdomains
        #[arg(long)]
        subdomains: bool,
        /// include, skip or only
        #[arg(long, default_value = "include")]
        sitemap: String,
        /// http, browser or auto
        #[arg(long, default_value = "auto")]
        engine: String,
        /// Follow bounded same-origin external JS candidates (requires --engine http)
        #[arg(long)]
        external_scripts: bool,
        /// Crawl the pages most relevant to this query first
        #[arg(long)]
        query: Option<String>,
        /// With --query: stop early once the collected pages cover the query
        #[arg(long, requires = "query")]
        adaptive: bool,
    },
    /// List the URLs of a site
    Map {
        url: String,
        #[arg(long, default_value_t = 5000)]
        limit: usize,
        /// Rank URLs matching these words first
        #[arg(long)]
        search: Option<String>,
        /// include, skip or only
        #[arg(long, default_value = "include")]
        sitemap: String,
    },
    /// Run the MCP server (stdio by default)
    Mcp {
        /// stdio or http
        #[arg(default_value = "stdio")]
        transport: String,
        /// Port for the http transport (listens on 127.0.0.1)
        #[arg(long, default_value_t = 3003)]
        port: u16,
    },
    /// Check the environment
    Doctor,
    /// Evaluate content extraction on local dataset snapshots
    Bench {
        /// Directory holding 1-0-0.csv and snapshots/ (see bench/fetch_snapshots.py)
        dir: PathBuf,
        /// Write per-page results as JSON lines
        #[arg(long)]
        out: Option<PathBuf>,
        /// Timing runs per page; the fastest is reported
        #[arg(long, default_value_t = 3)]
        runs: usize,
        /// Only evaluate the first N dataset rows
        #[arg(long)]
        limit: Option<usize>,
        /// Simulate `engine: auto` using browser-rendered snapshots (see bench-render)
        #[arg(long)]
        auto: bool,
        /// Write the HTML each page was evaluated on, plus a manifest, for other tools
        #[arg(long)]
        emit_inputs: Option<PathBuf>,
        /// Score another tool's output (JSON lines with id, markdown, ms) instead of pagesift
        #[arg(long)]
        external: Option<PathBuf>,
        /// Threads for the throughput run; 0 uses every core
        #[arg(long, default_value_t = 0)]
        jobs: usize,
    },
    /// Render thin snapshots with a browser for `bench --auto`
    #[command(hide = true)]
    BenchRender {
        dir: PathBuf,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
    },
}

fn main() -> ExitCode {
    pagesift_sys::tune_allocator();
    let cli = Cli::parse();
    let cfg = match pagesift::config::Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    // 结构化日志：PAGESIFT_LOG 定级别，PAGESIFT_LOG_FORMAT 或 [log] format 选 text / json
    let json = match cfg.log_json() {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let logs = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_env("PAGESIFT_LOG"))
        .with_writer(std::io::stderr);
    if json {
        logs.json().flatten_event(true).init();
    } else {
        logs.init();
    }
    match run(cli.command, cfg) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, mut cfg: pagesift::config::Config) -> Result<ExitCode> {
    match command {
        Command::Inspect { file, base_url } => {
            let html = read_bounded(file, 4 * 1024 * 1024)?;
            let forms =
                pagesift_extract::discover_forms(&html, &base_url).map_err(anyhow::Error::msg)?;
            let knowledge = pagesift_extract::KnowledgeExtractor::builtin()
                .extract(&html, &[])
                .map_err(anyhow::Error::msg)?;
            println!(
                "{}",
                serde_json::json!({"forms":forms,"knowledge":knowledge})
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Relocate {
            file,
            original,
            selector,
        } => {
            use pagesift_extract::{FuzzyAnchor, FuzzyMatch, FuzzyOptions};
            let old = read_bounded(Some(original), 4 * 1024 * 1024)?;
            let new = read_bounded(Some(file), 4 * 1024 * 1024)?;
            let old = pagesift_html::Html::parse(&old);
            let new = pagesift_html::Html::parse(&new);
            let anchor = FuzzyAnchor::capture(&old, &selector).map_err(anyhow::Error::msg)?;
            let result = match anchor
                .locate(&new, FuzzyOptions::default())
                .map_err(anyhow::Error::msg)?
            {
                FuzzyMatch::Candidate {
                    node,
                    score,
                    runner_up,
                } => {
                    serde_json::json!({"status":"candidate","score":score,"runner_up":runner_up,"text":node.text(),"html":node.html()})
                }
                FuzzyMatch::LowConfidence { best_score } => {
                    serde_json::json!({"status":"low_confidence","score":best_score})
                }
                FuzzyMatch::Ambiguous {
                    best_score,
                    runner_up,
                } => {
                    serde_json::json!({"status":"ambiguous","score":best_score,"runner_up":runner_up})
                }
                FuzzyMatch::BudgetExceeded => serde_json::json!({"status":"budget_exceeded"}),
            };
            println!("{result}");
            Ok(ExitCode::SUCCESS)
        }
        Command::DiscoverJs {
            url,
            max_files,
            max_depth,
        } => {
            let policy = configured_policy(&cfg)?;
            let graph = runtime()?.block_on(async {
                let (final_url, html) = fetch_text(&policy, &url, 4 * 1024 * 1024).await?;
                pagesift::scripts::discover_script_graph(
                    &policy,
                    &html,
                    &final_url,
                    pagesift::scripts::ScriptBudget {
                        max_files,
                        max_depth,
                        ..Default::default()
                    },
                )
                .await
            })?;
            println!("{}", serde_json::to_string(&graph)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::MediaExtract {
            file,
            base_url,
            adapter,
        } => {
            let body = read_bounded(file, 4 * 1024 * 1024)?;
            let items = pagesift::media::extract_media(adapter, &base_url, &body)?;
            println!("{}", serde_json::to_string(&items)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::MediaArchive {
            url,
            adapter,
            out,
            template,
            max_bytes,
        } => {
            anyhow::ensure!(
                (1..=2 * 1024 * 1024 * 1024).contains(&max_bytes),
                "media byte budget must be 1..=2 GiB"
            );
            let policy = configured_policy(&cfg)?;
            let results = runtime()?.block_on(async {
                let (url, body) = fetch_text(&policy, &url, 4 * 1024 * 1024).await?;
                let items = pagesift::media::extract_media(adapter, &url, &body)?;
                for item in &items {
                    pagesift::postprocess::media_filename(&template, item, &"0".repeat(64))?;
                }
                let mut archive = pagesift::download::Archive::open(out.join("archive"))?;
                let mut total = 0;
                let mut results = Vec::new();
                for item in items {
                    anyhow::ensure!(total < max_bytes, "media byte budget exhausted");
                    let request = pagesift::download::DownloadRequest {
                        source: item.source.clone(),
                        id: item.id.clone(),
                        revision: item.revision.clone(),
                        url: Url::parse(&item.url)?,
                        max_bytes: max_bytes - total,
                        expected_blake3: None,
                        restart: false,
                    };
                    let downloaded = archive.download(&policy, &request).await?;
                    total += downloaded.bytes;
                    results.push(pagesift::postprocess::publish_media(
                        &item,
                        &downloaded,
                        &out.join("files"),
                        &template,
                    )?);
                }
                Ok::<_, anyhow::Error>(results)
            })?;
            println!("{}", serde_json::to_string(&results)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::StreamPlan { file, base_url } => {
            let body = read_bounded(file, 1024 * 1024)?;
            println!(
                "{}",
                serde_json::to_string(&pagesift::stream::plan_stream(&body, &base_url)?)?
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::StreamDownload {
            url,
            out,
            variant,
            max_bytes,
        } => {
            let policy = configured_policy(&cfg)?;
            let results = runtime()?.block_on(async {
                let (mut url, body) = fetch_text(&policy, &url, 1024 * 1024).await?;
                let mut plan = pagesift::stream::plan_stream(&body, &url)?;
                if let pagesift::stream::StreamPlan::Master { variants } = &plan {
                    let index = variant.context("HLS master requires explicit --variant")?;
                    let next =
                        Url::parse(&variants.get(index).context("unknown variant index")?.url)?;
                    let (next, body) = fetch_text(&policy, &next, 1024 * 1024).await?;
                    url = next;
                    plan = pagesift::stream::plan_stream(&body, &url)?;
                } else {
                    anyhow::ensure!(variant.is_none(), "--variant requires a master playlist");
                }
                pagesift::stream::archive_stream(&policy, &plan, &url, &out, max_bytes).await
            })?;
            println!("{}", serde_json::to_string(&results)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::SessionFetch {
            url,
            state,
            reset_session,
            format,
            full,
        } => {
            let sc = cfg.scraper(false)?;
            let fetcher = Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?);
            let policy = pagesift_policy::Policy::new(fetcher, sc.policy);
            let state = pagesift::session::SessionFile::open(
                state,
                &url,
                Default::default(),
                reset_session,
            )?;
            let session = state.session();
            let response = runtime()?.block_on(policy.fetch(
                pagesift_net::FetchRequest::new(url.as_str()).with_session(session.clone()),
                pagesift_policy::RobotsMode::Respect,
            ));
            // Redirect cookies and error/retirement accounting also survive a failed request.
            state.save()?;
            let response = response?;
            anyhow::ensure!(
                (200..300).contains(&response.status),
                "HTTP status {}",
                response.status
            );
            anyhow::ensure!(!response.truncated, "session response exceeds body limit");
            let document = process(
                &response.text().text,
                &Options {
                    base_url: Some(response.final_url),
                    only_main_content: !full,
                    ..Default::default()
                },
            );
            if let Some(barrier) = &document.barrier {
                session.retire();
                state.save()?;
                anyhow::bail!("access barrier: {}", barrier.kind.as_str());
            }
            if format == "json" {
                println!(
                    "{}",
                    serde_json::json!({"markdown":document.markdown,"session":session.stats()})
                );
            } else {
                print!("{}", document.markdown);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Download {
            url,
            source,
            id,
            revision,
            out,
            max_bytes,
            expected_blake3,
            restart,
        } => {
            let sc = cfg.scraper(false)?;
            let fetcher = Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?);
            let policy = pagesift_policy::Policy::new(fetcher, sc.policy);
            let mut archive = pagesift::download::Archive::open(out)?;
            let request = pagesift::download::DownloadRequest {
                url,
                source,
                id,
                revision,
                max_bytes,
                expected_blake3,
                restart,
            };
            let result = runtime()?.block_on(archive.download(&policy, &request))?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Collect {
            url,
            source,
            data_dir,
            adapter,
            authoritative,
            max_pages,
            out,
            restart,
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
            let sc = cfg.scraper(false)?;
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
        } => {
            let html = read_input(file)?;
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
            allow_cidrs,
        } => runtime()?.block_on(async move {
            let mut sc = cfg.scraper(false)?;
            if let Some(p) = proxy {
                sc.fetcher.proxy = Some(p);
            }
            for cidr in allow_cidrs {
                if let Ok(parsed) = cidr.parse() {
                    sc.fetcher.ssrf.allow_cidrs.push(parsed);
                }
            }
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
            let result = scraper.scrape(&url, &opts).await;
            scraper.shutdown().await;
            match result {
                Ok(doc) if format == "json" => {
                    let body = serde_json::json!({ "success": true, "data": doc });
                    println!("{}", serde_json::to_string_pretty(&body)?);
                    Ok(ExitCode::SUCCESS)
                }
                Ok(doc) => {
                    print!("{}", doc.markdown.unwrap_or_default());
                    Ok(ExitCode::SUCCESS)
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
        Command::Serve {
            port,
            host,
            api_keys,
            public_url,
            data_dir,
            no_cache,
        } => runtime()?.block_on(async move {
            if data_dir.is_some() {
                cfg.cache.dir = data_dir;
            }
            // 缓存和任务共用数据目录里的一个存储
            let cache = !no_cache && cfg.cache.enabled.unwrap_or(true);
            let persist = cfg.server.persist_jobs.unwrap_or(true);
            let store = if cache || persist {
                Some(cfg.open_store()?)
            } else {
                None
            };
            let mut config = cfg.scraper(false)?;
            config.store = store.clone().filter(|_| cache);
            if store.is_some() {
                eprintln!(
                    "data: {} (cache {}, jobs {})",
                    cfg.data_dir().join("store").display(),
                    if cache { "on" } else { "off" },
                    if persist { "persisted" } else { "in memory" }
                );
            }
            let mut api = cfg.api()?;
            api.api_keys.extend(api_keys);
            if public_url.is_some() {
                api.public_url = public_url;
            }
            api.store = store.filter(|_| persist);
            let host = host
                .or(cfg.server.host.clone())
                .unwrap_or_else(|| "127.0.0.1".into());
            let port = port.or(cfg.server.port).unwrap_or(3002);
            let scraper = Arc::new(pagesift::Scraper::new(config)?);
            // 后台确认每个浏览器内核能不能启动，结果写进日志，不耽误服务开始监听
            let probe = scraper.clone();
            tokio::spawn(async move { probe.probe_browsers().await });
            let app = pagesift::api::router(scraper.clone(), api);
            let listener = tokio::net::TcpListener::bind((host.as_str(), port))
                .await
                .with_context(|| format!("binding {host}:{port}"))?;
            eprintln!("pagesift listening on http://{}", listener.local_addr()?);
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
            scraper.shutdown().await;
            Ok(ExitCode::SUCCESS)
        }),
        Command::Crawl {
            url,
            limit,
            out,
            depth,
            include,
            exclude,
            entire_domain,
            subdomains,
            sitemap,
            engine,
            external_scripts,
            query,
            adaptive,
        } => runtime()?.block_on(async move {
            let req: pagesift::api::types::CrawlRequest =
                serde_json::from_value(serde_json::json!({
                    "url": url,
                    "limit": limit,
                    "maxDiscoveryDepth": depth,
                    "includePaths": include,
                    "excludePaths": exclude,
                    "crawlEntireDomain": entire_domain,
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
            let scraper = pagesift::Scraper::new(cfg.scraper(false)?)?;
            let sink = Arc::new(cli::CrawlOutput::new(out));
            let result = scraper.crawl(&req, sink.clone()).await;
            scraper.shutdown().await;
            let (pages, errors) = sink.counts();
            eprintln!("crawled {pages} pages, {errors} errors");
            match result {
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
        } => runtime()?.block_on(async move {
            let req: pagesift::api::types::MapRequest =
                serde_json::from_value(serde_json::json!({
                    "url": url, "limit": limit, "search": search, "sitemap": sitemap,
                }))?;
            let scraper = pagesift::Scraper::new(cfg.scraper(false)?)?;
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
        Command::Doctor => runtime()?.block_on(cli::doctor(&cfg)),
        Command::Mcp { transport, port } => runtime()?.block_on(async move {
            let scraper = Arc::new(pagesift::Scraper::new(cfg.scraper(false)?)?);
            let res = pagesift::mcp::run_server(scraper.clone(), &transport, port).await;
            scraper.shutdown().await;
            res?;
            Ok(ExitCode::SUCCESS)
        }),
    }
}

/// Ctrl-C，或者 SIGTERM（`docker stop`、systemd）。进行中的任务已经落盘，下次启动接着跑。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutting down");
}

fn configured_policy(cfg: &pagesift::config::Config) -> Result<pagesift_policy::Policy> {
    let sc = cfg.scraper(false)?;
    Ok(pagesift_policy::Policy::new(
        Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?),
        sc.policy,
    ))
}

async fn fetch_text(
    policy: &pagesift_policy::Policy,
    url: &Url,
    max_bytes: usize,
) -> Result<(Url, String)> {
    let mut request = pagesift_net::FetchRequest::new(url.as_str());
    request.max_body_bytes = Some(max_bytes);
    let response = policy
        .fetch(request, pagesift_policy::RobotsMode::Respect)
        .await?;
    anyhow::ensure!(
        response.status == 200 && !response.truncated,
        "incomplete input or HTTP status {}",
        response.status
    );
    let decoded = response.text();
    anyhow::ensure!(!decoded.had_errors, "input contains undecodable bytes");
    Ok((response.final_url, decoded.text))
}

fn read_bounded(file: Option<PathBuf>, max_bytes: usize) -> Result<String> {
    let reader: Box<dyn Read> = match file {
        Some(p) if p.as_os_str() != "-" => Box::new(std::fs::File::open(p)?),
        _ => Box::new(std::io::stdin()),
    };
    let mut bytes = Vec::new();
    reader.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= max_bytes, "input exceeds byte budget");
    Ok(String::from_utf8(bytes)?)
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")
}

fn read_input(file: Option<PathBuf>) -> Result<String> {
    let bytes = match file {
        Some(path) if path.as_os_str() != "-" => {
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
        }
        _ => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("reading stdin")?;
            buf
        }
    };
    // 字符集识别在 M3 网络层做，这里先按 UTF-8 宽松解码
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
