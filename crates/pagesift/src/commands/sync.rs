//! `pagesift sync` subcommands: manage persistent Loop data sources and trigger incremental sync ticks.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use pagesift_api::Engine;
use pagesift_api::sources::{CreateSourceRequest, build_source_spec, validate_source_id};
use pagesift_store::{FreshnessPolicy, MAX_DELTAS_PER_SOURCE, Store, SyncBudget};

use crate::{AllowCidrs, runtime};

#[derive(Subcommand)]
pub(crate) enum SyncCommand {
    /// Register or update a persistent Loop data source
    Init {
        /// Store directory (defaults to `<data_dir>/store`)
        #[arg(long)]
        store: Option<PathBuf>,
        /// Source identifier (optional; auto-generated when omitted)
        #[arg(long)]
        id: Option<String>,
        /// Human-readable name
        #[arg(long)]
        name: Option<String>,
        /// Seed HTTP(S) URLs (pass multiple `--seed` flags)
        #[arg(long = "seed", required = true)]
        seeds: Vec<String>,
        /// Allowed URL path prefixes
        #[arg(long = "include-path")]
        include_paths: Vec<String>,
        /// Excluded URL path prefixes
        #[arg(long = "exclude-path")]
        exclude_paths: Vec<String>,
        #[arg(long, default_value_t = 2)]
        max_depth: usize,
        #[arg(long)]
        allow_subdomains: bool,
        #[arg(long, default_value_t = 60)]
        min_interval_secs: u64,
        #[arg(long, default_value_t = 86_400)]
        max_interval_secs: u64,
        #[arg(long, default_value_t = 3_600)]
        default_interval_secs: u64,
        #[arg(long, default_value_t = 2)]
        confirm_gone_after_404s: u32,
        #[arg(long, default_value_t = 50)]
        max_requests_per_tick: usize,
        #[arg(long, default_value_t = 5)]
        max_browser_renders_per_tick: usize,
        #[arg(long, default_value_t = 60_000)]
        max_wall_clock_ms: u64,
        #[arg(long)]
        target_items: Option<usize>,
        #[arg(long = "require-field")]
        required_fields: Vec<String>,
        /// Disable adaptive browser fallback for this source
        #[arg(long)]
        no_browser: bool,
        #[arg(long)]
        json: bool,
    },
    /// List registered Loop data sources in the store
    List {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Execute one bounded adaptive sync tick for a data source
    Run {
        #[arg(long)]
        store: Option<PathBuf>,
        /// Source id to synchronize
        id: String,
        #[arg(long)]
        force_all: bool,
        #[arg(long)]
        max_requests: Option<usize>,
        #[command(flatten)]
        allow: AllowCidrs,
        #[arg(long)]
        json: bool,
    },
    /// Show concise status and latest tick summary for a data source
    Status {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect full resource inventory, active claims, and conflicts (supports `--as-of`)
    State {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List recent tick deltas or inspect one tick delta by `--tick-id`
    Deltas {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long)]
        tick_id: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Remove a data source from the store
    Delete {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long)]
        json: bool,
    },
}

fn open_sync_store(cfg: &pagesift::config::Config, custom: Option<&Path>) -> Result<Store> {
    let dir = custom
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.data_dir().join("store"));
    Store::open(pagesift_store::Config::new(&dir))
        .with_context(|| format!("opening store at {}", dir.display()))
}

fn parse_as_of_opt(raw: Option<&str>) -> Result<Option<u64>> {
    let Some(s) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if let Ok(ms) = s.parse::<u64>() {
        return Ok(Some(ms));
    }
    pagesift_graph::parse_timestamp_ms(s)
        .map(Some)
        .with_context(|| format!("invalid --as-of timestamp {s:?}; expected Unix ms or RFC-3339"))
}

pub(crate) fn run(action: SyncCommand, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match action {
        SyncCommand::Init {
            store,
            id,
            name,
            seeds,
            include_paths,
            exclude_paths,
            max_depth,
            allow_subdomains,
            min_interval_secs,
            max_interval_secs,
            default_interval_secs,
            confirm_gone_after_404s,
            max_requests_per_tick,
            max_browser_renders_per_tick,
            max_wall_clock_ms,
            target_items,
            required_fields,
            no_browser,
            json,
        } => {
            let req = CreateSourceRequest {
                id,
                name,
                seeds,
                include_paths,
                exclude_paths,
                max_depth,
                allow_subdomains,
                freshness: FreshnessPolicy {
                    min_interval_secs,
                    max_interval_secs,
                    default_interval_secs,
                    confirm_gone_after_404s,
                },
                budget: SyncBudget {
                    max_requests_per_tick,
                    max_browser_renders_per_tick,
                    max_wall_clock_ms,
                    target_items,
                },
                extract_schema: None,
                extract_prompt: None,
                required_fields,
                allow_browser: !no_browser,
            };
            let mut spec = build_source_spec(req, None)
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let st = open_sync_store(&cfg, store.as_deref())?;
            let sync = st.sync();
            if let Some(prev) = sync.get_source(&spec.id)? {
                spec.created_at_ms = prev.created_at_ms;
            }
            sync.put_source(&spec)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&spec)?);
            } else {
                println!("source: {} (seeds={})", spec.id, spec.seeds.len());
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncCommand::List { store, json } => {
            let st = open_sync_store(&cfg, store.as_deref())?;
            let sources = st.sync().list_sources()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&sources)?);
            } else {
                println!("sources: {}", sources.len());
                for s in &sources {
                    println!(
                        "  {}  seeds={}  max_req={}  browser={}",
                        s.id,
                        s.seeds.len(),
                        s.budget.max_requests_per_tick,
                        s.allow_browser
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncCommand::Run {
            store,
            id,
            force_all,
            max_requests,
            allow,
            json,
        } => {
            let id = validate_source_id(&id)
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let st = open_sync_store(&cfg, store.as_deref())?;
            let delta = runtime()?.block_on(async move {
                let mut sc = cfg.scraper(false)?;
                allow.apply(&mut sc);
                sc.store = Some(st);
                let scraper = pagesift::Scraper::new(sc)?;
                let res = Engine::sync_source(&scraper, &id, None, force_all, max_requests).await;
                scraper.shutdown().await;
                res.map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&delta)?);
            } else {
                println!(
                    "tick={} coverage={} executed={} added={} modified={} migrated={} disappeared={}",
                    delta.tick_id,
                    delta.coverage_status,
                    delta.stats.requests_executed,
                    delta.added_resources.len(),
                    delta.modified_resources.len(),
                    delta.migrated_resources.len(),
                    delta.disappeared_resources.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncCommand::Status { store, id, json } => {
            let id = validate_source_id(&id)
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let st = open_sync_store(&cfg, store.as_deref())?;
            let view = st
                .sync()
                .build_state_view(&id, None)?
                .with_context(|| format!("data source '{id}' not found"))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": view.spec.id,
                        "lastTickId": view.last_tick_id,
                        "lastSyncedAtMs": view.last_synced_at_ms,
                        "totalTicks": view.total_ticks,
                        "coverageStatus": view.coverage_status,
                        "inventoryCounts": view.inventory_counts,
                        "activeClaims": view.active_claims.len(),
                        "conflicts": view.conflicts.len(),
                    }))?
                );
            } else {
                println!(
                    "source={} ticks={} coverage={} active_claims={} conflicts={}",
                    view.spec.id,
                    view.total_ticks,
                    view.coverage_status.as_deref().unwrap_or("none"),
                    view.active_claims.len(),
                    view.conflicts.len()
                );
                for (state, count) in &view.inventory_counts {
                    println!("  {state}: {count}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncCommand::State {
            store,
            id,
            as_of,
            json,
        } => {
            let id = validate_source_id(&id)
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let cutoff = parse_as_of_opt(as_of.as_deref())?;
            let st = open_sync_store(&cfg, store.as_deref())?;
            let view = st
                .sync()
                .build_state_view(&id, cutoff)?
                .with_context(|| format!("data source '{id}' not found"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!(
                    "source={} resources={} active_claims={} conflicts={} coverage={}",
                    view.spec.id,
                    view.resources.len(),
                    view.active_claims.len(),
                    view.conflicts.len(),
                    view.coverage_status.as_deref().unwrap_or("none")
                );
                for r in &view.resources {
                    println!("  [{}] {} (role={:?})", r.state.as_str(), r.url, r.role);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncCommand::Deltas {
            store,
            id,
            tick_id,
            limit,
            json,
        } => {
            let id = validate_source_id(&id)
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let st = open_sync_store(&cfg, store.as_deref())?;
            let sync = st.sync();
            if sync.get_source(&id)?.is_none() {
                bail!("data source '{id}' not found");
            }
            if let Some(tick) = tick_id {
                let delta = sync
                    .get_delta(&id, &tick)?
                    .with_context(|| format!("delta '{tick}' for source '{id}' not found"))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&delta)?);
                } else {
                    println!(
                        "{} coverage={} executed={} added={} modified={} transitions={}",
                        delta.tick_id,
                        delta.coverage_status,
                        delta.stats.requests_executed,
                        delta.added_resources.len(),
                        delta.modified_resources.len(),
                        delta.claim_transitions.len()
                    );
                }
                return Ok(ExitCode::SUCCESS);
            }
            let deltas = sync.list_deltas(&id, limit.clamp(1, MAX_DELTAS_PER_SOURCE))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&deltas)?);
            } else {
                println!("deltas: {}", deltas.len());
                for d in &deltas {
                    println!(
                        "  {} ts={} coverage={} reqs={} +{} ~{} -{}",
                        d.tick_id,
                        d.completed_at_ms,
                        d.coverage_status,
                        d.stats.requests_executed,
                        d.added_resources.len(),
                        d.modified_resources.len(),
                        d.disappeared_resources.len()
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncCommand::Delete { store, id, json } => {
            let id = validate_source_id(&id)
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let st = open_sync_store(&cfg, store.as_deref())?;
            let deleted = st.sync().delete_source(&id)?;
            if !deleted {
                bail!("data source '{id}' not found");
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": id,
                        "deleted": true,
                    }))?
                );
            } else {
                println!("deleted: {id}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_sync_init_list_state_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let cfg = pagesift::config::Config::default();

        assert_eq!(
            run(
                SyncCommand::Init {
                    store: Some(store_path.clone()),
                    id: Some("pricing".into()),
                    name: Some("Pricing".into()),
                    seeds: vec!["https://example.com/pricing".into()],
                    include_paths: Vec::new(),
                    exclude_paths: Vec::new(),
                    max_depth: 2,
                    allow_subdomains: false,
                    min_interval_secs: 60,
                    max_interval_secs: 3600,
                    default_interval_secs: 600,
                    confirm_gone_after_404s: 2,
                    max_requests_per_tick: 10,
                    max_browser_renders_per_tick: 2,
                    max_wall_clock_ms: 30_000,
                    target_items: Some(5),
                    required_fields: vec!["price".into()],
                    no_browser: false,
                    json: true,
                },
                pagesift::config::Config::default(),
            )
            .unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run(
                SyncCommand::State {
                    store: Some(store_path.clone()),
                    id: "pricing".into(),
                    as_of: Some("2026-01-01T00:00:00Z".into()),
                    json: true,
                },
                pagesift::config::Config::default(),
            )
            .unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run(
                SyncCommand::Delete {
                    store: Some(store_path),
                    id: "pricing".into(),
                    json: true,
                },
                cfg,
            )
            .unwrap(),
            ExitCode::SUCCESS
        );
    }
}
