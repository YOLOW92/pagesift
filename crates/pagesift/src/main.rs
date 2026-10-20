#![forbid(unsafe_code)]

mod bench;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

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
    Serve,
    /// Scrape a single URL
    Scrape,
    /// Crawl a site
    Crawl,
    /// List the URLs of a site
    Map,
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
        Command::Serve
        | Command::Scrape
        | Command::Crawl
        | Command::Map
        | Command::Mcp
        | Command::Doctor => {
            eprintln!("not implemented yet");
            Ok(ExitCode::from(2))
        }
    }
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
