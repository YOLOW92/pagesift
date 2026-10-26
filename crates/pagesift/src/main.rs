#![forbid(unsafe_code)]

#[macro_use]
mod output;
mod args;
mod bench;
mod cli;
mod commands;
mod util;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pagesift_content::{Options, process};
use tracing_subscriber::EnvFilter;
use url::Url;

use args::*;
use util::*;

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

fn run(command: Command, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match command {
        command @ (Command::Inspect { .. }
        | Command::Relocate { .. }
        | Command::DiscoverJs { .. }
        | Command::MediaExtract { .. }
        | Command::MediaArchive { .. }
        | Command::StreamPlan { .. }
        | Command::StreamDownload { .. }
        | Command::SessionFetch { .. }
        | Command::Download { .. }) => commands::assets::run(command, cfg),
        command @ (Command::Collect { .. }
        | Command::Convert { .. }
        | Command::Bench { .. }
        | Command::BenchRender { .. }
        | Command::Scrape { .. }
        | Command::Crawl { .. }
        | Command::Map { .. }) => commands::pages::run(command, cfg),
        command @ (Command::Serve { .. }
        | Command::Diagnose { .. }
        | Command::Tune { .. }
        | Command::Doctor
        | Command::Mcp { .. }) => commands::service::run(command, cfg),
    }
}
