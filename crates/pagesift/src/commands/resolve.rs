//! `pagesift resolve` subcommands: evidence-driven verification (`check`), persisted conclusion
//! inspection (`list`, `get`, `history`, `graph`, `delete`), Live incremental maintenance
//! (`reconcile`, `revalidate`, `events`), and conservative calibration (`learn`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand};
use pagesift_api::Engine;
use pagesift_store::Store;
use serde_json::{Value, json};

use crate::commands::context::{load_local_files, open_context_store};
use crate::{AllowCidrs, runtime};

#[derive(Args)]
pub(crate) struct ResolveCheckCmd {
    #[arg(long)]
    store: Option<PathBuf>,
    /// Candidate answer text to decompose and verify
    #[arg(long)]
    answer: Option<String>,
    /// Explicit atomic claims to verify (repeatable)
    #[arg(long = "claim")]
    claims: Vec<String>,
    /// Optional original question context
    #[arg(long)]
    query: Option<String>,
    /// Optional domain bucket for calibration
    #[arg(long)]
    domain: Option<String>,
    /// Context/Loop source IDs to verify against (repeatable)
    #[arg(long = "source-id")]
    source_ids: Vec<String>,
    /// Optional inline local files for ephemeral verification without pre-ingestion
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Explicit conclusion ID when updating an existing persisted conclusion
    #[arg(long)]
    conclusion_id: Option<String>,
    /// Upstream parent conclusion IDs (repeatable; cycles rejected)
    #[arg(long = "parent")]
    parent_conclusion_ids: Vec<String>,
    /// Explicit assumptions required by this conclusion (repeatable)
    #[arg(long = "assumption")]
    assumptions: Vec<String>,
    #[arg(
        long,
        default_value = "preponderance",
        value_parser = ["scintilla", "preponderance", "clear_and_convincing", "strict_universal"]
    )]
    proof_standard: String,
    #[arg(
        long,
        default_value = "open_world",
        value_parser = ["open_world", "closed_observed_set"]
    )]
    domain_kind: String,
    #[arg(
        long,
        default_value = "deterministic_only",
        value_parser = ["deterministic_only", "hybrid_assisted"]
    )]
    verifier_mode: String,
    #[arg(long, default_value_t = 1)]
    min_independent_sources: usize,
    #[arg(long)]
    as_of: Option<String>,
    #[arg(long)]
    effective_at: Option<String>,
    #[arg(long)]
    include_unverified: bool,
    #[arg(long)]
    no_gap_resolution: bool,
    #[arg(long)]
    no_counter_search: bool,
    #[arg(long)]
    allow_network: bool,
    #[arg(long)]
    allow_external_search: bool,
    #[arg(long)]
    strict_constrain_to_urls: bool,
    #[arg(long = "url")]
    candidate_urls: Vec<String>,
    #[arg(long = "allowed-host")]
    allowed_hosts: Vec<String>,
    #[arg(long, default_value_t = 4)]
    max_requests: usize,
    #[arg(long)]
    persist: bool,
    #[arg(long)]
    no_learn: bool,
    #[command(flatten)]
    allow: AllowCidrs,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
pub(crate) enum ResolveCommand {
    /// Verify a candidate answer or explicit atomic claims against grounded Context or local files
    Check(Box<ResolveCheckCmd>),
    /// List persisted Resolve conclusions
    List {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        source_id: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Fetch a persisted conclusion (latest, by --version, or point-in-time --as-of)
    Get {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long)]
        version: Option<u64>,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// View immutable revision history for a persisted conclusion
    History {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long, default_value_t = 32)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Inspect conclusion dependency graph, provenance semiring, and Minimal Cut Sets
    Graph {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Reconcile ContextDeltas into Resolve (Early Cutoff, ScopeDependency, ABA fallback)
    Reconcile {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long = "source-id")]
        source_ids: Vec<String>,
        #[arg(
            long,
            default_value = "incremental",
            value_parser = ["incremental", "full_source"]
        )]
        mode: String,
        #[arg(long)]
        json: bool,
    },
    /// Revalidate stale or specified conclusions (incremental or full)
    Revalidate {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long = "id")]
        conclusion_ids: Vec<String>,
        #[arg(long = "source-id")]
        source_ids: Vec<String>,
        #[arg(
            long,
            default_value = "incremental",
            value_parser = ["incremental", "full"]
        )]
        mode: String,
        #[arg(
            long,
            default_value = "deterministic_only",
            value_parser = ["deterministic_only", "hybrid_assisted"]
        )]
        verifier_mode: String,
        #[arg(long)]
        allow_network: bool,
        #[arg(long)]
        allow_external_search: bool,
        #[arg(long = "allowed-host")]
        allowed_hosts: Vec<String>,
        #[arg(long, default_value_t = 4)]
        max_requests: usize,
        #[command(flatten)]
        allow: AllowCidrs,
        #[arg(long)]
        json: bool,
    },
    /// Poll monotonic conclusion lifecycle events
    Events {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long)]
        after_seq: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or update conservative domain-stratified Resolve calibration state
    Learn {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
        #[arg(long, conflicts_with = "unfreeze")]
        freeze: bool,
        #[arg(long)]
        unfreeze: bool,
        #[arg(long)]
        reset: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete a persisted conclusion and its reverse dependency edges
    Delete {
        #[arg(long)]
        store: Option<PathBuf>,
        id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

fn execute_resolve_request<F, Fut>(
    cfg: &pagesift::config::Config,
    store: Option<Store>,
    allow: Option<&AllowCidrs>,
    build_req: F,
) -> Result<Value>
where
    F: FnOnce(pagesift::Scraper) -> Fut,
    Fut: std::future::Future<Output = Result<(pagesift::Scraper, Value)>>,
{
    let mut sc = cfg.scraper(false)?;
    if let Some(a) = allow {
        a.apply(&mut sc);
    }
    sc.store = store;
    runtime()?.block_on(async move {
        let scraper = pagesift::Scraper::new(sc)?;
        let (scraper, req) = build_req(scraper).await?;
        let res = Engine::resolve(&scraper, None, req).await;
        scraper.shutdown().await;
        res.map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))
    })
}

pub(crate) fn run(action: ResolveCommand, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match action {
        ResolveCommand::Check(cmd) => {
            let ResolveCheckCmd {
                store,
                answer,
                claims,
                query,
                domain,
                source_ids,
                files,
                conclusion_id,
                parent_conclusion_ids,
                assumptions,
                proof_standard,
                domain_kind,
                verifier_mode,
                min_independent_sources,
                as_of,
                effective_at,
                include_unverified,
                no_gap_resolution,
                no_counter_search,
                allow_network,
                allow_external_search,
                strict_constrain_to_urls,
                candidate_urls,
                allowed_hosts,
                max_requests,
                persist,
                no_learn,
                allow,
                json,
            } = *cmd;
            if answer.as_deref().is_none_or(|s| s.trim().is_empty()) && claims.is_empty() {
                anyhow::bail!("resolve check requires --answer or at least one --claim");
            }
            let st = if persist || files.is_empty() || store.is_some() {
                Some(open_context_store(&cfg, store.as_deref())?)
            } else {
                None
            };
            let out = execute_resolve_request(&cfg, st, Some(&allow), |scraper| async move {
                let docs = if files.is_empty() {
                    Vec::new()
                } else {
                    load_local_files(&scraper, &files, None, None, None, None, None).await?
                };
                let mut req = json!({
                    "action": "check",
                    "answer": answer.unwrap_or_default(),
                    "claims": claims,
                    "query": query,
                    "domain": domain,
                    "sourceIds": source_ids,
                    "conclusionId": conclusion_id,
                    "parentConclusionIds": parent_conclusion_ids,
                    "assumptions": assumptions,
                    "proofStandard": proof_standard,
                    "domainKind": domain_kind,
                    "verifierMode": verifier_mode,
                    "minIndependentSources": min_independent_sources,
                    "asOf": as_of,
                    "effectiveAt": effective_at,
                    "includeUnverified": include_unverified,
                    "enableGapResolution": !no_gap_resolution,
                    "enableCounterSearch": !no_counter_search,
                    "allowNetworkAcquisition": allow_network,
                    "allowExternalSearch": allow_external_search,
                    "strictConstrainToUrls": strict_constrain_to_urls,
                    "candidateUrls": candidate_urls,
                    "allowedHosts": allowed_hosts,
                    "maxAcquisitionRequests": max_requests,
                    "persist": persist,
                    "learnEnabled": !no_learn,
                });
                if !docs.is_empty() {
                    req["documents"] = serde_json::to_value(docs)?;
                }
                Ok((scraper, req))
            })?;
            print_resolve_output(&out, json, "check")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::List {
            store,
            state,
            source_id,
            limit,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "conclusions",
                        "state": state,
                        "sourceId": source_id,
                        "limit": limit,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "list")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Get {
            store,
            id,
            version,
            as_of,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "get",
                        "conclusionId": id,
                        "version": version,
                        "asOf": as_of,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "get")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::History {
            store,
            id,
            limit,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "history",
                        "conclusionId": id,
                        "limit": limit,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "history")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Graph {
            store,
            id,
            limit,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "graph",
                        "conclusionId": id,
                        "limit": limit,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "graph")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Reconcile {
            store,
            source_ids,
            mode,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "reconcile",
                        "sourceIds": source_ids,
                        "mode": mode,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "reconcile")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Revalidate {
            store,
            conclusion_ids,
            source_ids,
            mode,
            verifier_mode,
            allow_network,
            allow_external_search,
            allowed_hosts,
            max_requests,
            allow,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, Some(&allow), |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "revalidate",
                        "conclusionIds": conclusion_ids,
                        "sourceIds": source_ids,
                        "mode": mode,
                        "verifierMode": verifier_mode,
                        "allowNetworkAcquisition": allow_network,
                        "allowExternalSearch": allow_external_search,
                        "allowedHosts": allowed_hosts,
                        "maxAcquisitionRequests": max_requests,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "revalidate")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Events {
            store,
            after_seq,
            limit,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "events",
                        "afterSeq": after_seq,
                        "limit": limit,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "events")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Learn {
            store,
            enable,
            disable,
            freeze,
            unfreeze,
            reset,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                let enabled = if enable {
                    Some(true)
                } else if disable {
                    Some(false)
                } else {
                    None
                };
                let frozen = if freeze {
                    Some(true)
                } else if unfreeze {
                    Some(false)
                } else {
                    None
                };
                Ok((
                    scraper,
                    json!({
                        "action": "learn",
                        "enabled": enabled,
                        "frozen": frozen,
                        "reset": reset,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "learn")?;
            Ok(ExitCode::SUCCESS)
        }
        ResolveCommand::Delete {
            store,
            id,
            reason,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_resolve_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "delete",
                        "conclusionId": id,
                        "reason": reason,
                    }),
                ))
            })?;
            print_resolve_output(&out, json, "delete")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn print_resolve_output(out: &Value, as_json: bool, kind: &str) -> Result<()> {
    if !as_json
        && kind == "check"
        && let Some(c) = out.get("outcome").and_then(|o| o.get("conclusion"))
    {
        let cid = c.get("conclusionId").and_then(Value::as_str).unwrap_or("");
        let state = c.get("state").and_then(Value::as_str).unwrap_or("unknown");
        let sup = c
            .get("independentSupportRoots")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let refu = c
            .get("independentRefuteRoots")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let gaps = c
            .get("unresolvedGaps")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        eprintln!(
            "[resolve check: id={cid} state={state} support_roots={sup} refute_roots={refu} unresolved_gaps={gaps}]"
        );
    }
    println!("{}", serde_json::to_string_pretty(out)?);
    Ok(())
}
