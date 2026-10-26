#![forbid(unsafe_code)]

#[macro_use]
mod output;
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
    /// Config file (default: $PAGESIFT_CONFIG, else the platform config path; empty disables config files)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Append one JSON line per fetch (URL, engine, status, timings, warnings) to this file, and keep HTML snapshots of suspicious results next to it
    #[arg(long, global = true, value_name = "PATH")]
    audit_log: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// 细粒度内网放行：HTTP 抓取和浏览器子请求共用同一份名单。
#[derive(clap::Args)]
struct AllowCidrs {
    /// Allow private CIDR subnets (e.g. 10.0.0.0/8, 198.18.0.0/15) for HTTP fetching and browser rendering
    #[arg(long = "allow-cidr", value_name = "CIDR", value_parser = parse_cidr)]
    allow_cidrs: Vec<pagesift_net::IpCidr>,
}

impl AllowCidrs {
    fn apply(&self, config: &mut pagesift::ScraperConfig) {
        config
            .fetcher
            .ssrf
            .allow_cidrs
            .extend(self.allow_cidrs.iter().cloned());
    }
}

fn parse_cidr(raw: &str) -> Result<pagesift_net::IpCidr, String> {
    raw.parse()
        .map_err(|error| format!("invalid CIDR {raw:?}: {error}"))
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
        /// Decode the input with this charset (e.g. Shift_JIS, GBK) instead of detecting it
        #[arg(long)]
        charset: Option<String>,
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
        #[command(flatten)]
        allow: AllowCidrs,
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
        #[command(flatten)]
        allow: AllowCidrs,
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
        #[command(flatten)]
        allow: AllowCidrs,
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
        #[command(flatten)]
        allow: AllowCidrs,
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
        #[command(flatten)]
        allow: AllowCidrs,
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
        /// Crawl the whole domain, not just pages under the start path (same as --scope domain)
        #[arg(long)]
        entire_domain: bool,
        /// Path scope: auto (start directory, widened one level when the start page is a leaf
        /// such as a blog post), exact (never widen), parent (one level up) or domain
        #[arg(long, default_value = "auto", value_parser = ["auto", "exact", "parent", "domain"])]
        scope: String,
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
        /// Tuning mode: adaptive (senses hardware and dynamically scales) or conservative
        #[arg(long, default_value = "adaptive")]
        tuning_mode: String,
        #[command(flatten)]
        allow: AllowCidrs,
    },
    /// Proactively explore host machine's performance curve and output an autonomous tuning profile
    Tune {
        /// Target profile intensity: standard, aggressive, or extreme
        #[arg(long, default_value = "extreme")]
        profile: String,
        /// File to write the JSON profile to (prints to stdout if omitted)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Diagnose current performance bottlenecks, latency distributions, and hardware saturation
    Diagnose {
        /// Include deep performance saturation and latency quantile analysis
        #[arg(long)]
        performance: bool,
        /// Output diagnosis report in JSON format
        #[arg(long)]
        json: bool,
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
        #[command(flatten)]
        allow: AllowCidrs,
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

/// 抓取输出层要显式告知的 warning：空正文、auto 降级、HTTP 错误状态。
fn print_scrape_warnings(doc: &pagesift::api::types::Document) {
    let Some(p) = doc.metadata.pagesift.as_ref() else {
        return;
    };
    for warning in &p.warnings {
        if warning.starts_with("empty_content:")
            || warning.starts_with("render_unavailable")
            || warning.starts_with("render_skipped_")
            || warning.starts_with("http_error_status:")
            || warning.starts_with("http_redirect_status:")
            || warning.starts_with("not_modified:")
        {
            eprintln!("warning: {warning}");
        }
    }
}

/// 页面抓到了，但 HTTP 状态说明这不是一次成功的内容获取：5xx，404 或没跟随成功的 3xx 且正文为空。
/// 退出码 2 便于脚本区分“请求本身失败”（1）和“服务器返回错误页”（2）。
fn scrape_exit_code(doc: &pagesift::api::types::Document) -> ExitCode {
    let status = doc.metadata.status_code;
    let empty = doc.markdown.as_deref().is_none_or(|m| m.trim().is_empty());
    if status >= 500 || ((status == 404 || (300..400).contains(&status)) && empty) {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    pagesift_sys::tune_allocator();
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let cli = match <Cli as clap::FromArgMatches>::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    let mut cfg = match pagesift::config::Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    if cli.audit_log.is_some() {
        cfg.audit.path = cli.audit_log;
    }
    cfg.audit.source = matches.subcommand_name().map(String::from);
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
            allow,
        } => {
            let mut sc = cfg.scraper(false)?;
            allow.apply(&mut sc);
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
            allow,
        } => {
            let mut sc = cfg.scraper(false)?;
            allow.apply(&mut sc);
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
            let result = scraper.scrape(&url, &opts).await;
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
        Command::Serve {
            port,
            host,
            api_keys,
            public_url,
            data_dir,
            no_cache,
            allow,
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
            allow.apply(&mut config);
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
            pagesift::api::serve(listener, app, shutdown_signal()).await?;
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
        Command::Diagnose { performance, json } => runtime()?.block_on(async move {
            let engine = pagesift::adaptive::AutonomousTuningEngine::new();
            let diagnosis = engine.diagnose();
            if json {
                println!("{}", serde_json::to_string_pretty(&diagnosis)?);
            } else {
                println!("============================================================");
                println!(
                    "  PageSift {} Performance & Hardware Diagnosis",
                    env!("CARGO_PKG_VERSION")
                );
                println!("============================================================");
                println!("  Logical CPUs:       {}", diagnosis.logical_cores);
                println!(
                    "  Physical Cores:     {} ({} capacity cores, {} runtime workers)",
                    diagnosis.physical_cores, diagnosis.worker_cores, diagnosis.runtime_workers
                );
                println!(
                    "  Topology:           {} socket(s), {} NUMA node(s), {} thread(s)/core",
                    diagnosis.sockets, diagnosis.numa_nodes, diagnosis.threads_per_core
                );
                println!(
                    "  System Memory:      {:.1} MB (Process RSS: {:.1} MB)",
                    diagnosis.total_memory_mb, diagnosis.process_rss_mb
                );
                println!("  CPU Saturation:     {:.1}%", diagnosis.cpu_saturation_pct);
                println!("  Process CPU Time:   {:.3}s", diagnosis.process_cpu_time_s);
                println!("  FD Limit (ulimit):  {}", diagnosis.fd_limit);
                println!("  Active Concurrency: {}", diagnosis.active_concurrency);
                println!("  Active Domains:     {}", diagnosis.active_sites);
                if performance {
                    println!(
                        "  Dispatch Latency:   p50: {:.2}ms | p95: {:.2}ms | p99: {:.2}ms",
                        diagnosis.p50_latency_ms,
                        diagnosis.p95_latency_ms,
                        diagnosis.p99_latency_ms
                    );
                }
                println!("------------------------------------------------------------");
                println!("  Primary State:      {}", diagnosis.primary_bottleneck);
                println!("  Recommendation:     {}", diagnosis.recommendation);
                println!("============================================================");
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Tune { profile, out } => runtime()?.block_on(async move {
            println!("PageSift Autonomous Performance Profiler");
            println!("Exploring hardware response curve with profile: {profile}...");
            let engine = pagesift::adaptive::AutonomousTuningEngine::new();
            let knobs = engine.tick(0.0, 0.0, 0);
            let profile_json = serde_json::to_string_pretty(&serde_json::json!({
                "profile_mode": profile,
                "hardware": {
                    "logical_cores": engine.topo.logical_cores,
                    "physical_cores": engine.topo.physical_cores,
                    "worker_cores": engine.topo.worker_cores,
                    "runtime_workers": engine.topo.runtime_workers,
                    "sockets": engine.topo.sockets,
                    "numa_nodes": engine.topo.numa_nodes,
                    "threads_per_core": engine.topo.threads_per_core,
                    "ram_per_core_mb": engine.topo.ram_per_core_mb,
                    "fd_limit": engine.topo.fd_limit,
                    "worker_cpu_groups": engine.topo.cpu_topology.physical_worker_groups(),
                },
                "calibrated_knobs": knobs,
            }))?;

            if let Some(path) = out {
                std::fs::write(&path, &profile_json)?;
                println!(
                    "Tuning profile successfully calibrated and saved to {}",
                    path.display()
                );
            } else {
                println!("{profile_json}");
            }
            Ok(ExitCode::SUCCESS)
        }),
        Command::Doctor => runtime()?.block_on(cli::doctor(&cfg)),
        Command::Mcp { transport, port } => runtime()?.block_on(async move {
            let mut cfg = cfg;
            cfg.audit.source = Some(format!("mcp-{transport}"));
            let mut sc = cfg.scraper(false)?;
            // http 传输是一个常驻进程，按配置开缓存；stdio 每个客户端各起一个进程，存储不能多进程共用，
            // 不开缓存。数据目录被别的进程（比如 serve）占着时照常运行，只是不缓存
            if transport == "http" && cfg.cache.enabled.unwrap_or(true) {
                match cfg.open_store() {
                    Ok(store) => {
                        eprintln!("cache: {}", cfg.data_dir().join("store").display());
                        sc.store = Some(store);
                    }
                    Err(e)
                        if e.downcast_ref::<pagesift_store::Error>()
                            .is_some_and(pagesift_store::Error::is_locked) =>
                    {
                        eprintln!("warning: {e:#}; running without the cache");
                    }
                    Err(e) => return Err(e),
                }
            }
            let scraper = Arc::new(pagesift::Scraper::new(sc)?);
            let res = pagesift::mcp::run_server_until(
                scraper.clone(),
                &transport,
                port,
                shutdown_signal(),
            )
            .await;
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

/// 每个 runtime worker 预留的虚拟地址空间（线程栈 + 运行时结构）。
/// 受限地址空间下按这个粒度折算可用线程数。
const ADDR_SPACE_PER_WORKER: u64 = 16 * 1024 * 1024;

/// 读 `/proc/self/limits` 的 "Max address space" 与当前 `VmSize`，
/// 在地址空间受限（如 `ulimit -v`）时给出还能开几个 worker。
/// 不受限或读不到时返回 `None`。
fn addr_space_worker_cap() -> Option<usize> {
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = limits
        .lines()
        .find(|l| l.starts_with("Max address space"))?;
    let soft = line.split_whitespace().nth(3)?;
    if soft == "unlimited" {
        return None;
    }
    let limit: u64 = soft.parse().ok()?;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let vm: u64 = status
        .lines()
        .find(|l| l.starts_with("VmSize:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?;
    let vm_bytes = vm * 1024;
    let free = limit.saturating_sub(vm_bytes);
    Some(((free / ADDR_SPACE_PER_WORKER) as usize).max(1))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    let topology = pagesift::adaptive::HardwareTopology::detect();
    let worker_cpus = topology.cpu_topology.runtime_worker_cpus();
    let mut workers = topology.runtime_workers.max(1);
    if let Some(cap) = addr_space_worker_cap() {
        workers = workers.min(cap);
    }
    // 必须在绑核之前：之后各线程只看得到自己那一个 CPU。
    pagesift::set_cpu_capacity(topology.runtime_workers);
    // 地址空间不够时线程会 spawn 失败（EAGAIN）。逐次减半重试，而不是让运行时 panic
    // 或卡在没有输出的状态里——受限环境下至少还能以小并发把活干完。
    loop {
        let next_cpu = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .worker_threads(workers)
            .thread_name("pagesift-worker");
        if !worker_cpus.is_empty() {
            let cpus = worker_cpus.clone();
            builder.on_thread_start(move || {
                use std::sync::atomic::Ordering;
                // 运行时先起 worker，之后按需起的是阻塞池线程（正文提取、PDF 解析）。
                // 只绑 worker：阻塞线程交给调度器放到空闲核上，免得和某个 worker 挤在同一个核。
                let index = next_cpu.fetch_add(1, Ordering::Relaxed);
                if index < workers {
                    let _ = pagesift::adaptive::pin_current_thread(cpus[index % cpus.len()]);
                }
            });
        }
        match builder.enable_all().build() {
            Ok(rt) => {
                if workers < topology.runtime_workers {
                    eprintln!(
                        "note: 地址空间受限，runtime worker 由 {} 降到 {workers}",
                        topology.runtime_workers
                    );
                }
                return Ok(rt);
            }
            Err(e) => {
                if workers <= 1 {
                    return Err(anyhow::anyhow!(
                        "starting the async runtime failed even with 1 worker: {e}"
                    ));
                }
                workers = (workers / 2).max(1);
            }
        }
    }
}

fn read_input(
    file: Option<PathBuf>,
    charset: Option<&str>,
) -> Result<(String, bool, &'static str)> {
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
    decode_input(&bytes, charset)
}

/// 解码本地 HTML：显式 --charset 优先，否则复用网络抓取路径的编码判定
/// （BOM → meta 预扫描 → 检测）。返回文本、是否有无法解码的字节、最终编码名。
fn decode_input(bytes: &[u8], charset: Option<&str>) -> Result<(String, bool, &'static str)> {
    if let Some(label) = charset {
        let label = label.trim();
        let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown charset {label:?}; use a WHATWG encoding label such as UTF-8, Shift_JIS or GBK"
            )
        })?;
        let (text, _, had_errors) = encoding.decode(bytes);
        return Ok((text.into_owned(), had_errors, encoding.name()));
    }
    let decoded = pagesift_net::decode(bytes, None, None);
    Ok((decoded.text, decoded.had_errors, decoded.encoding_name))
}

#[cfg(test)]
mod tests {
    use super::decode_input;

    #[test]
    fn decodes_local_html_from_meta_charset() {
        let text = "日本語の記事です。文字化けせずに読めます。";
        let html = format!(
            r#"<html><head><meta http-equiv="Content-Type" content="text/html; charset=Shift_JIS"></head><body><p>{text}</p></body></html>"#
        );
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&html);
        let (decoded, had_errors, encoding) = decode_input(bytes.as_ref(), None).unwrap();
        assert!(decoded.contains(text), "decoded as {encoding}: {decoded}");
        assert!(!had_errors);
        assert_eq!(encoding, "Shift_JIS");
    }

    #[test]
    fn explicit_charset_overrides_meta() {
        let text = "这是中文测试";
        let html = format!(
            r#"<html><head><meta charset="utf-8"></head><body><p>{text}</p></body></html>"#
        );
        let (bytes, _, _) = encoding_rs::GBK.encode(&html);
        let (decoded, had_errors, encoding) = decode_input(bytes.as_ref(), Some("gbk")).unwrap();
        assert!(decoded.contains(text));
        assert!(!had_errors);
        assert_eq!(encoding, "GBK");
    }

    #[test]
    fn rejects_unknown_charset() {
        let err = decode_input(b"<html></html>", Some("not-a-charset")).unwrap_err();
        assert!(err.to_string().contains("unknown charset"));
    }
}
