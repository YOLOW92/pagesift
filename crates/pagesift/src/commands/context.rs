//! `pagesift context` subcommands: ingest multi-format documents, reconcile Loop sources,
//! run evidence-grounded Search & Pack compilation, execute bounded Context Actions,
//! poll/ack ContextDeltas, and inspect/manage drift-guarded online Learn calibration.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;
use pagesift_api::Engine;
use pagesift_api::types::{ScrapeOptions, Upload, UploadKind};
use pagesift_engine::context::ContextDocumentInput;
use pagesift_store::Store;
use serde_json::{Value, json};
use url::Url;

use crate::{AllowCidrs, runtime};

#[derive(Subcommand)]
pub(crate) enum ContextCommand {
    /// Ingest local files (HTML, PDF, Office, Markdown, code) or URLs into Context units & deltas
    Ingest {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(long, default_value = "default")]
        source_id: String,
        /// Local file paths to parse and ingest (repeatable)
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        /// HTTP(S) URLs to scrape and ingest (repeatable)
        #[arg(long = "url")]
        urls: Vec<String>,
        /// Explicit base/source URL when ingesting a single local file
        #[arg(long)]
        base_url: Option<Url>,
        #[arg(long)]
        subject_key: Option<String>,
        #[arg(long)]
        valid_from: Option<String>,
        #[arg(long)]
        valid_until: Option<String>,
        #[arg(long)]
        origin_root: Option<String>,
        #[arg(
            long,
            default_value = "boundedUot",
            value_parser = ["exactAnchorOnly", "greedyDice", "boundedUot"]
        )]
        match_mode: String,
        #[arg(long)]
        ephemeral: bool,
        #[command(flatten)]
        allow: AllowCidrs,
        #[arg(long)]
        json: bool,
    },
    /// Reconcile a persisted 1.5 Loop data source into 1.6 Context units and deltas
    Sync {
        #[arg(long)]
        store: Option<PathBuf>,
        /// Persisted Loop source identifier
        id: String,
        #[arg(
            long,
            default_value = "boundedUot",
            value_parser = ["exactAnchorOnly", "greedyDice", "boundedUot"]
        )]
        match_mode: String,
        #[arg(long)]
        json: bool,
    },
    /// Multi-strategy evidence-, lineage-, and time-aware Context retrieval
    Search {
        #[arg(long)]
        store: Option<PathBuf>,
        query: String,
        #[arg(long = "source-id")]
        source_ids: Vec<String>,
        /// Optional inline local files for ephemeral search without pre-ingestion
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        #[arg(long)]
        subject_key: Option<String>,
        #[arg(long = "require-field")]
        required_fields: Vec<String>,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        effective_at: Option<String>,
        #[arg(long, default_value_t = 16)]
        limit: usize,
        #[arg(long)]
        include_unverified: bool,
        #[arg(long)]
        no_conflicts: bool,
        #[arg(long)]
        no_semantic_sketch: bool,
        #[arg(long)]
        no_learn: bool,
        #[arg(long)]
        json: bool,
    },
    /// Compile a budget- and MustKeep-constrained ContextPack with ProvenanceManifest
    Pack {
        #[arg(long)]
        store: Option<PathBuf>,
        query: String,
        #[arg(long = "source-id")]
        source_ids: Vec<String>,
        /// Optional inline local files for ephemeral ContextPack compilation
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        #[arg(long, default_value_t = 2048)]
        budget_tokens: usize,
        #[arg(
            long,
            default_value = "submodularClosure",
            value_parser = ["submodularClosure", "boundedDpp", "mmrGreedy", "topKGreedy"]
        )]
        selector: String,
        #[arg(
            long,
            default_value = "edgeSalience",
            value_parser = ["edgeSalience", "scoreDescending", "documentOrder"]
        )]
        ordering: String,
        #[arg(long)]
        subject_key: Option<String>,
        #[arg(long = "require-field")]
        required_fields: Vec<String>,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        effective_at: Option<String>,
        #[arg(long, default_value_t = 16)]
        limit: usize,
        #[arg(long)]
        no_learn: bool,
        #[arg(long)]
        json: bool,
    },
    /// Execute a goal-oriented Context Action (context, research, verify, watch, changes)
    Action {
        #[arg(long)]
        store: Option<PathBuf>,
        #[arg(
            long,
            default_value = "context",
            value_parser = ["context", "research", "verify", "watch", "changes"]
        )]
        mode: String,
        #[arg(default_value = "")]
        query: String,
        #[arg(long = "source-id")]
        source_ids: Vec<String>,
        #[arg(long = "url")]
        urls: Vec<String>,
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        #[arg(long, default_value_t = 2048)]
        budget_tokens: usize,
        #[arg(
            long,
            default_value = "submodularClosure",
            value_parser = ["submodularClosure", "boundedDpp", "mmrGreedy", "topKGreedy"]
        )]
        selector: String,
        #[arg(
            long,
            default_value = "edgeSalience",
            value_parser = ["edgeSalience", "scoreDescending", "documentOrder"]
        )]
        ordering: String,
        #[arg(long, default_value_t = 1)]
        min_independent_sources: usize,
        #[arg(long)]
        max_staleness_ms: Option<u64>,
        #[arg(long)]
        allow_network: bool,
        #[arg(long)]
        allow_external_search: bool,
        #[arg(long)]
        strict_constrain_to_urls: bool,
        #[arg(long = "allowed-host")]
        allowed_hosts: Vec<String>,
        #[arg(long, default_value_t = 4)]
        max_requests: usize,
        #[arg(long)]
        since_delta_seq: Option<u64>,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        effective_at: Option<String>,
        #[arg(long)]
        no_learn: bool,
        #[command(flatten)]
        allow: AllowCidrs,
        #[arg(long)]
        json: bool,
    },
    /// List persisted KnowledgeUnits and IndexVisibility for a Context source
    Units {
        #[arg(long)]
        store: Option<PathBuf>,
        source_id: String,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Poll unacknowledged ContextDeltas for an external consumer checkpoint
    Deltas {
        #[arg(long)]
        store: Option<PathBuf>,
        source_id: String,
        #[arg(long, default_value = "default-consumer")]
        consumer_id: String,
        #[arg(long)]
        spec_fingerprint: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Acknowledge processed ContextDeltas for an external consumer checkpoint
    Ack {
        #[arg(long)]
        store: Option<PathBuf>,
        source_id: String,
        #[arg(long, default_value = "default-consumer")]
        consumer_id: String,
        #[arg(long)]
        instance_id: String,
        #[arg(long)]
        seq: u64,
        #[arg(long, default_value = "")]
        spec_fingerprint: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or configure drift-guarded online Context calibration
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
        #[arg(
            long,
            value_parser = ["cusum", "cusumPageHinkley", "bocpd", "bernoulliBocpd", "none"]
        )]
        drift_guard: Option<String>,
        #[arg(long)]
        raw_score: Option<f64>,
        #[arg(long)]
        grounded_correct: Option<bool>,
        #[arg(long)]
        hard_ground_truth: bool,
        #[arg(long)]
        json: bool,
    },
}

fn open_context_store(cfg: &pagesift::config::Config, custom: Option<&Path>) -> Result<Store> {
    let dir = custom
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.data_dir().join("store"));
    Store::open(pagesift_store::Config::new(&dir))
        .with_context(|| format!("opening store at {}", dir.display()))
}

fn encode_url_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{b:02X}");
        }
    }
    if out.is_empty() {
        "document".to_owned()
    } else {
        out
    }
}

fn short_path_disambiguator(path: &Path) -> String {
    // [协议] FNV-1a 64 位标准初始偏移基数与素数乘子（用于多目录同名本地文件 URL 路径确定性消歧）
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in path.as_os_str().as_encoded_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3); // [协议] FNV-1a 64 位素数乘子
    }
    format!("{h:016x}")[..8].to_owned()
}

fn build_local_fallback_url(
    path: &Path,
    filename: &str,
    single_base_url: Option<&Url>,
    duplicate_filename: bool,
) -> (Option<Url>, String) {
    if let Some(u) = single_base_url {
        return (Some(u.clone()), u.to_string());
    }
    let encoded_name = encode_url_segment(filename);
    let url_str = if duplicate_filename {
        let prefix = short_path_disambiguator(path);
        format!("https://local.invalid/{prefix}/{encoded_name}")
    } else {
        format!("https://local.invalid/{encoded_name}")
    };
    (Url::parse(&url_str).ok(), url_str)
}

async fn load_local_files(
    scraper: &pagesift::Scraper,
    files: &[PathBuf],
    base_url: Option<&Url>,
    subject_key: Option<&str>,
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    origin_root: Option<&str>,
) -> Result<Vec<ContextDocumentInput>> {
    let mut docs = Vec::with_capacity(files.len());
    let opts = ScrapeOptions {
        graph: Some(pagesift_graph::GraphOptions::enabled()),
        max_age: Some(0),
        ..ScrapeOptions::default()
    };
    let mut name_counts = std::collections::HashMap::<String, usize>::with_capacity(files.len());
    for p in files {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_owned();
        *name_counts.entry(name).or_insert(0) += 1;
    }

    for path in files {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading local input file {}", path.display()))?;
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_owned();
        let single_base = if files.len() == 1 { base_url } else { None };
        let duplicate_name = name_counts.get(&filename).copied().unwrap_or(0) > 1;
        let (file_url, fallback_url) =
            build_local_fallback_url(path, &filename, single_base, duplicate_name);

        let mut item = if let Some(kind) = UploadKind::detect(&bytes, &filename, None) {
            let parsed = scraper
                .parse(
                    Upload {
                        bytes,
                        filename,
                        content_type: None,
                        kind,
                        url: file_url,
                    },
                    &opts,
                )
                .await
                .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
            let mut input = ContextDocumentInput::from_api_document(&parsed, &fallback_url);
            if !input.url.contains("://") {
                input.url = fallback_url;
            }
            input
        } else {
            let markdown = String::from_utf8(bytes).with_context(|| {
                format!(
                    "unsupported binary format and non-UTF8 content in {}",
                    path.display()
                )
            })?;
            ContextDocumentInput {
                url: fallback_url,
                markdown,
                title: Some(filename),
                source_kind: Some(pagesift_graph::SourceKind::Other {
                    label: "text".to_owned(),
                }),
                observed_at_ms: None,
                valid_from: None,
                valid_until: None,
                subject_key: None,
                origin_root: None,
                origin_independent: true,
                status_code: Some(200), // [协议] 本地文件视为 HTTP 200 OK 等价可用文档
                unreliable: false,
                confirmed_gone: false,
            }
        };

        if let Some(sk) = subject_key {
            item.subject_key = Some(sk.to_owned());
        }
        if let Some(vf) = valid_from {
            item.valid_from = Some(vf.to_owned());
        }
        if let Some(vu) = valid_until {
            item.valid_until = Some(vu.to_owned());
        }
        if let Some(oroot) = origin_root {
            item.origin_root = Some(oroot.to_owned());
        }
        docs.push(item);
    }
    Ok(docs)
}

async fn scrape_urls_to_inputs(
    scraper: &pagesift::Scraper,
    urls: &[String],
    subject_key: Option<&str>,
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    origin_root: Option<&str>,
) -> Result<Vec<ContextDocumentInput>> {
    let mut out = Vec::with_capacity(urls.len());
    let opts = ScrapeOptions {
        graph: Some(pagesift_graph::GraphOptions::enabled()),
        max_age: Some(0),
        ..ScrapeOptions::default()
    };
    for url in urls {
        let doc = Engine::scrape(scraper, url, &opts)
            .await
            .map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))?;
        let mut item = ContextDocumentInput::from_api_document(&doc, url);
        if let Some(sk) = subject_key {
            item.subject_key = Some(sk.to_owned());
        }
        if let Some(vf) = valid_from {
            item.valid_from = Some(vf.to_owned());
        }
        if let Some(vu) = valid_until {
            item.valid_until = Some(vu.to_owned());
        }
        if let Some(oroot) = origin_root {
            item.origin_root = Some(oroot.to_owned());
        }
        out.push(item);
    }
    Ok(out)
}

fn execute_context_request<F, Fut>(
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
        let res = Engine::context(&scraper, None, req).await;
        scraper.shutdown().await;
        res.map_err(|e| anyhow::anyhow!("{}: {}", e.code.as_str(), e.message))
    })
}

fn maybe_open_store_for_query(
    cfg: &pagesift::config::Config,
    store: Option<&Path>,
    has_inline_files: bool,
) -> Result<Option<Store>> {
    if !has_inline_files || store.is_some() {
        Ok(Some(open_context_store(cfg, store)?))
    } else {
        Ok(None)
    }
}

pub(crate) fn run(action: ContextCommand, cfg: pagesift::config::Config) -> Result<ExitCode> {
    match action {
        ContextCommand::Ingest {
            store,
            source_id,
            files,
            urls,
            base_url,
            subject_key,
            valid_from,
            valid_until,
            origin_root,
            match_mode,
            ephemeral,
            allow,
            json,
        } => {
            let st = if ephemeral {
                None
            } else {
                Some(open_context_store(&cfg, store.as_deref())?)
            };
            let out = execute_context_request(&cfg, st, Some(&allow), |scraper| async move {
                let mut docs = load_local_files(
                    &scraper,
                    &files,
                    base_url.as_ref(),
                    subject_key.as_deref(),
                    valid_from.as_deref(),
                    valid_until.as_deref(),
                    origin_root.as_deref(),
                )
                .await?;
                let scraped = scrape_urls_to_inputs(
                    &scraper,
                    &urls,
                    subject_key.as_deref(),
                    valid_from.as_deref(),
                    valid_until.as_deref(),
                    origin_root.as_deref(),
                )
                .await?;
                docs.extend(scraped);
                let req = json!({
                    "action": "ingest",
                    "sourceId": source_id,
                    "persist": !ephemeral,
                    "matchMode": match_mode,
                    "documents": docs,
                });
                Ok((scraper, req))
            })?;
            print_output(&out, json, "ingest")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Sync {
            store,
            id,
            match_mode,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "sync",
                        "sourceId": id,
                        "matchMode": match_mode,
                    }),
                ))
            })?;
            print_output(&out, json, "sync")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Search {
            store,
            query,
            source_ids,
            files,
            subject_key,
            required_fields,
            as_of,
            effective_at,
            limit,
            include_unverified,
            no_conflicts,
            no_semantic_sketch,
            no_learn,
            json,
        } => {
            let st = maybe_open_store_for_query(&cfg, store.as_deref(), !files.is_empty())?;
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
                let docs = load_local_files(&scraper, &files, None, None, None, None, None).await?;
                let req = json!({
                    "action": "search",
                    "query": query,
                    "sourceIds": source_ids,
                    "documents": if docs.is_empty() { Value::Null } else { serde_json::to_value(docs)? },
                    "subjectKey": subject_key,
                    "requiredFields": required_fields,
                    "asOf": as_of,
                    "effectiveAt": effective_at,
                    "limit": limit,
                    "includeUnverified": include_unverified,
                    "includeConflicts": !no_conflicts,
                    "enableSemanticSketch": !no_semantic_sketch,
                    "learnEnabled": !no_learn,
                });
                Ok((scraper, req))
            })?;
            print_output(&out, json, "search")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Pack {
            store,
            query,
            source_ids,
            files,
            budget_tokens,
            selector,
            ordering,
            subject_key,
            required_fields,
            as_of,
            effective_at,
            limit,
            no_learn,
            json,
        } => {
            let st = maybe_open_store_for_query(&cfg, store.as_deref(), !files.is_empty())?;
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
                let docs = load_local_files(&scraper, &files, None, None, None, None, None).await?;
                let req = json!({
                    "action": "pack",
                    "query": query,
                    "sourceIds": source_ids,
                    "documents": if docs.is_empty() { Value::Null } else { serde_json::to_value(docs)? },
                    "budgetTokens": budget_tokens,
                    "selector": selector,
                    "ordering": ordering,
                    "subjectKey": subject_key,
                    "requiredFields": required_fields,
                    "asOf": as_of,
                    "effectiveAt": effective_at,
                    "limit": limit,
                    "learnEnabled": !no_learn,
                });
                Ok((scraper, req))
            })?;
            print_output(&out, json, "pack")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Action {
            store,
            mode,
            query,
            source_ids,
            urls,
            files,
            budget_tokens,
            selector,
            ordering,
            min_independent_sources,
            max_staleness_ms,
            allow_network,
            allow_external_search,
            strict_constrain_to_urls,
            allowed_hosts,
            max_requests,
            since_delta_seq,
            as_of,
            effective_at,
            no_learn,
            allow,
            json,
        } => {
            let st = maybe_open_store_for_query(&cfg, store.as_deref(), !files.is_empty())?;
            let out = execute_context_request(&cfg, st, Some(&allow), |scraper| async move {
                let docs = load_local_files(&scraper, &files, None, None, None, None, None).await?;
                let req = json!({
                    "action": "action",
                    "mode": mode,
                    "query": query,
                    "sourceIds": source_ids,
                    "candidateUrls": urls,
                    "documents": if docs.is_empty() { Value::Null } else { serde_json::to_value(docs)? },
                    "budgetTokens": budget_tokens,
                    "selector": selector,
                    "ordering": ordering,
                    "minIndependentSources": min_independent_sources,
                    "maxStalenessMs": max_staleness_ms,
                    "allowNetworkAcquisition": allow_network,
                    "allowExternalSearch": allow_external_search,
                    "strictConstrainToUrls": strict_constrain_to_urls,
                    "allowedHosts": allowed_hosts,
                    "maxAcquisitionRequests": max_requests,
                    "sinceDeltaSeq": since_delta_seq,
                    "asOf": as_of,
                    "effectiveAt": effective_at,
                    "learnEnabled": !no_learn,
                });
                Ok((scraper, req))
            })?;
            print_output(&out, json, "action")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Units {
            store,
            source_id,
            url,
            limit,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "units",
                        "sourceId": source_id,
                        "url": url,
                        "limit": limit,
                    }),
                ))
            })?;
            print_output(&out, json, "units")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Deltas {
            store,
            source_id,
            consumer_id,
            spec_fingerprint,
            limit,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "deltas",
                        "sourceId": source_id,
                        "consumerId": consumer_id,
                        "specFingerprint": spec_fingerprint,
                        "limit": limit,
                    }),
                ))
            })?;
            print_output(&out, json, "deltas")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Ack {
            store,
            source_id,
            consumer_id,
            instance_id,
            seq,
            spec_fingerprint,
            json,
        } => {
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
                Ok((
                    scraper,
                    json!({
                        "action": "ack",
                        "sourceId": source_id,
                        "consumerId": consumer_id,
                        "instanceId": instance_id,
                        "ackedDeltaSeq": seq,
                        "specFingerprint": spec_fingerprint,
                    }),
                ))
            })?;
            print_output(&out, json, "ack")?;
            Ok(ExitCode::SUCCESS)
        }
        ContextCommand::Learn {
            store,
            enable,
            disable,
            freeze,
            unfreeze,
            drift_guard,
            raw_score,
            grounded_correct,
            hard_ground_truth,
            json,
        } => {
            if raw_score.is_none() && (grounded_correct.is_some() || hard_ground_truth) {
                anyhow::bail!(
                    "--raw-score is required when --grounded-correct or --hard-ground-truth is set"
                );
            }
            let st = Some(open_context_store(&cfg, store.as_deref())?);
            let out = execute_context_request(&cfg, st, None, |scraper| async move {
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
                let feedback = raw_score.map(|rs| {
                    json!({
                        "rawScore": rs,
                        "groundedCorrect": grounded_correct.unwrap_or(false),
                        "isHardGroundTruth": hard_ground_truth,
                    })
                });
                Ok((
                    scraper,
                    json!({
                        "action": "learn",
                        "enabled": enabled,
                        "frozen": frozen,
                        "driftGuard": drift_guard,
                        "feedback": feedback,
                    }),
                ))
            })?;
            print_output(&out, json, "learn")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn print_output(out: &Value, as_json: bool, kind: &str) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(out)?);
        return Ok(());
    }
    match kind {
        "pack" => {
            if let Some(pack) = out.get("pack") {
                let text = pack
                    .get("compiledContext")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("{text}");
                let tokens = pack.get("totalTokens").and_then(Value::as_u64).unwrap_or(0);
                let budget = pack
                    .get("effectiveBudgetTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let refs = pack
                    .get("provenanceManifest")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                eprintln!("[context pack: tokens={tokens}/{budget} citations={refs}]");
                return Ok(());
            }
        }
        "action" => {
            if let Some(outcome) = out.get("outcome") {
                let stop = outcome
                    .get("stoppingReason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let reused = outcome
                    .get("reusedExistingContext")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(text) = outcome
                    .get("pack")
                    .and_then(|p| p.get("compiledContext"))
                    .and_then(Value::as_str)
                    && !text.is_empty()
                {
                    println!("{text}");
                }
                eprintln!("[context action: stopping={stop} reused={reused}]");
                return Ok(());
            }
        }
        _ => {}
    }
    println!("{}", serde_json::to_string_pretty(out)?);
    Ok(())
}
