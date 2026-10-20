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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    /// Start the Firecrawl-compatible HTTP API
    Serve {
        #[arg(long, default_value_t = 3002)]
        port: u16,
        /// Address to listen on
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
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
    /// Run the MCP server
    Mcp,
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
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_env("PAGESIFT_LOG"))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<ExitCode> {
    match command {
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
        } => runtime()?.block_on(async move {
            let scraper = pagesift::Scraper::new(pagesift::ScraperConfig::default())?;
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
            mut api_keys,
            public_url,
            data_dir,
            no_cache,
        } => runtime()?.block_on(async move {
            if let Ok(keys) = std::env::var("PAGESIFT_API_KEYS") {
                api_keys.extend(
                    keys.split(',')
                        .map(str::trim)
                        .filter(|k| !k.is_empty())
                        .map(String::from),
                );
            }
            let mut config = pagesift::ScraperConfig::default();
            if !no_cache {
                let dir = data_dir.unwrap_or_else(default_data_dir).join("store");
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
                config.store = Some(
                    pagesift_store::Store::open_dir(&dir)
                        .with_context(|| format!("opening the cache in {}", dir.display()))?,
                );
                eprintln!("cache: {}", dir.display());
            }
            let scraper = Arc::new(pagesift::Scraper::new(config)?);
            let app = pagesift::api::router(
                scraper.clone(),
                pagesift::api::Config {
                    api_keys,
                    public_url,
                    max_batch_concurrency: None,
                },
            );
            let listener = tokio::net::TcpListener::bind((host.as_str(), port))
                .await
                .with_context(|| format!("binding {host}:{port}"))?;
            eprintln!("pagesift listening on http://{}", listener.local_addr()?);
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
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
                    "scrapeOptions": { "engine": engine },
                }))?;
            if let Some(dir) = &out {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            let scraper = pagesift::Scraper::new(pagesift::ScraperConfig::default())?;
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
            let scraper = pagesift::Scraper::new(pagesift::ScraperConfig::default())?;
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
        Command::Doctor => runtime()?.block_on(cli::doctor()),
        Command::Mcp => {
            eprintln!("not implemented yet");
            Ok(ExitCode::from(2))
        }
    }
}

pub(crate) fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("PAGESIFT_DATA_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("pagesift")
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
