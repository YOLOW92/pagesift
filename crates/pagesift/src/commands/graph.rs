//! `pagesift graph inspect` subcommand: inspect crawl output artifacts or persisted DataGraph stores.

use std::path::Path;

use anyhow::{Context, bail};
use clap::Subcommand;
use pagesift_graph::{GraphOptions, GraphSummary, PageGraphDelta, StreamingGraphBuilder};

use crate::*;

#[derive(Subcommand)]
pub(crate) enum GraphCommand {
    /// Inspect DataGraph artifacts in a crawl `--out` directory, graph JSON/JSONL file, or data store directory
    Inspect {
        /// Path to `--out` directory, `graph_summary.json` / `graph.jsonl` file, or store data directory
        path: PathBuf,
        /// Filter version history by URL or normalized resource key (for store or delta inspection)
        #[arg(long)]
        url: Option<String>,
        /// Filter recorded conflicts or claims by entity subject key (e.g. sku:PRO-1)
        #[arg(long)]
        subject: Option<String>,
        /// Filter historical store state at or before Unix ms or RFC-3339 timestamp
        #[arg(long)]
        as_of: Option<String>,
        /// Output raw inspection result as JSON
        #[arg(long)]
        json: bool,
    },
}

fn parse_as_of_cli(raw: Option<&str>) -> Result<Option<u64>> {
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

pub(crate) fn run(action: GraphCommand, _cfg: pagesift::config::Config) -> Result<ExitCode> {
    match action {
        GraphCommand::Inspect {
            path,
            url,
            subject,
            as_of,
            json,
        } => inspect(
            &path,
            url.as_deref(),
            subject.as_deref(),
            as_of.as_deref(),
            json,
        ),
    }
}

fn inspect(
    path: &Path,
    url: Option<&str>,
    subject: Option<&str>,
    as_of: Option<&str>,
    as_json: bool,
) -> Result<ExitCode> {
    let as_of_ms = parse_as_of_cli(as_of)?;
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }

    if path.is_file() {
        let summary = if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            read_jsonl_summary(path)?
        } else {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading graph summary {}", path.display()))?;
            serde_json::from_slice::<GraphSummary>(&bytes)
                .with_context(|| format!("parsing GraphSummary from {}", path.display()))?
        };
        return emit_summary(&summary, url, subject, as_json);
    }

    let summary_file = path.join("graph_summary.json");
    let deltas_file = path.join("graph.jsonl");
    if summary_file.exists() {
        let bytes = std::fs::read(&summary_file)
            .with_context(|| format!("reading {}", summary_file.display()))?;
        let summary = serde_json::from_slice::<GraphSummary>(&bytes)
            .with_context(|| format!("parsing {}", summary_file.display()))?;
        return emit_summary(&summary, url, subject, as_json);
    }
    if deltas_file.exists() {
        let summary = read_jsonl_summary(&deltas_file)?;
        return emit_summary(&summary, url, subject, as_json);
    }

    let store = pagesift_store::Store::open_dir(path)
        .with_context(|| format!("opening store at {}", path.display()))?;
    inspect_store(&store, url, subject, as_of_ms, as_json)
}

fn read_jsonl_summary(path: &Path) -> Result<GraphSummary> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading graph deltas {}", path.display()))?;
    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let delta: PageGraphDelta = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing PageGraphDelta at line {}", idx + 1))?;
        builder.ingest_delta(delta);
    }
    Ok(builder.finalize())
}

fn emit_summary(
    summary: &GraphSummary,
    url: Option<&str>,
    subject: Option<&str>,
    as_json: bool,
) -> Result<ExitCode> {
    let filtered_conflicts: Vec<_> = summary
        .conflicts
        .iter()
        .filter(|c| subject.is_none_or(|s| c.subject_key == s))
        .cloned()
        .collect();
    if let Some(target) = url {
        let key = if target.contains("://") {
            pagesift_graph::normalize_resource_key(target)
        } else {
            target.to_string()
        };
        let nodes: Vec<_> = summary
            .nodes
            .iter()
            .filter(|n| n.resource_key == key)
            .collect();
        let edges: Vec<_> = summary
            .versions
            .iter()
            .filter(|e| e.resource_key == key)
            .collect();
        if as_json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "resourceKey": key,
                    "nodes": nodes,
                    "versions": edges,
                    "conflicts": filtered_conflicts,
                }))?
            );
        } else {
            println!("resource_key: {key}");
            println!(
                "nodes: {}, version_edges: {}, conflicts: {}",
                nodes.len(),
                edges.len(),
                filtered_conflicts.len()
            );
            for n in nodes {
                println!(
                    "  {} status={} ts={} title={:?}",
                    n.final_url, n.status_code, n.observed_at_ms, n.title
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if as_json {
        if subject.is_some() {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "subject": subject,
                    "conflicts": filtered_conflicts,
                }))?
            );
        } else {
            println!("{}", serde_json::to_string_pretty(summary)?);
        }
        return Ok(ExitCode::SUCCESS);
    }
    let st = &summary.stats;
    println!(
        "nodes: {}  blocks: {}  citations: {}  provenance: {}  versions: {}  evidence: {}  claims: {}  fact_clusters: {}  conflicts: {}  canonical_clusters: {}",
        st.nodes,
        st.blocks,
        st.citations,
        st.provenance_edges,
        st.version_edges,
        st.evidence_spans,
        st.claims,
        st.fact_clusters,
        st.conflicts,
        st.canonical_clusters
    );
    for c in &filtered_conflicts {
        println!(
            "conflict [{}] {} .{}: {} variants (preferred={:?})",
            c.conflict_id,
            c.subject_key,
            c.field_path,
            c.variants.len(),
            c.preferred_hint
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn inspect_store(
    store: &pagesift_store::Store,
    url: Option<&str>,
    subject: Option<&str>,
    as_of_ms: Option<u64>,
    as_json: bool,
) -> Result<ExitCode> {
    let gs = store.graph();
    let max_limit = pagesift_store::DEFAULT_GRAPH_CONFLICTS_LIMIT;
    let (conflicts, _, _) = gs.list_conflicts_as_of(subject, as_of_ms, max_limit, 0)?;
    if let Some(target) = url {
        let hist = gs.history_as_of(
            target,
            as_of_ms,
            pagesift_store::DEFAULT_GRAPH_HISTORY_LIMIT,
        )?;
        let mut claims = gs.claims_for_resource_as_of(target, as_of_ms)?;
        if let Some(sk) = subject {
            claims.retain(|c| c.claim.subject_key == sk);
        }
        if as_json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "resourceKey": hist.resource_key,
                    "asOfMs": as_of_ms,
                    "versions": hist.versions,
                    "edges": hist.edges,
                    "claims": claims,
                    "conflicts": conflicts,
                }))?
            );
        } else {
            println!("resource_key: {}", hist.resource_key);
            println!(
                "versions: {}, edges: {}, claims: {}, conflicts: {}",
                hist.versions.len(),
                hist.edges.len(),
                claims.len(),
                conflicts.len()
            );
            for v in &hist.versions {
                println!(
                    "  {} doc={} status={} ts={}",
                    v.url, v.doc_id, v.status_code, v.observed_at_ms
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let (claims, _, _) = gs.list_claims_bounded(subject, as_of_ms, max_limit, 0)?;
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "subject": subject,
                "asOfMs": as_of_ms,
                "totalClaims": claims.len(),
                "claims": claims,
                "totalConflicts": conflicts.len(),
                "conflicts": conflicts,
            }))?
        );
    } else {
        println!("claims: {}, conflicts: {}", claims.len(), conflicts.len());
        for c in &conflicts {
            println!(
                "conflict [{}] {} .{}: {} variants",
                c.conflict_id,
                c.subject_key,
                c.field_path,
                c.variants.len()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_graph::{DocumentInput, SourceKind};

    fn doc(url: &str, hash: &str, ts: u64) -> DocumentInput {
        DocumentInput {
            requested_url: url.into(),
            final_url: url.into(),
            canonical_url: None,
            canonical_from_header: false,
            source_kind: SourceKind::Html,
            observed_at_ms: ts,
            status_code: 200,
            engine: "http".into(),
            content_hash: Some(hash.into()),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("Home".into()),
            markdown: "# Home".into(),
            redirect_chain: Vec::new(),
            links: Vec::new(),
            alternates: Vec::new(),
            duplicate_of: None,
            attachments: Vec::new(),
            structured_json_ld: Vec::new(),
            extracted_fields: None,
            raw_traces: Vec::new(),
            chunk_spans: Vec::new(),
            previous_version: None,
            version_diff_stats: None,
            changed_block_ids: Vec::new(),
        }
    }

    #[test]
    fn inspect_reads_directory_jsonl_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
        let d1 = builder.ingest(doc("https://example.com/", "h1", 100));
        let d2 = builder.ingest(doc("https://example.com/about", "h2", 200));
        let summary = builder.finalize();

        let jsonl = format!(
            "{}\n{}\n",
            serde_json::to_string(&d1).unwrap(),
            serde_json::to_string(&d2).unwrap(),
        );
        let jsonl_path = dir.path().join("graph.jsonl");
        std::fs::write(&jsonl_path, jsonl).unwrap();

        let from_jsonl = read_jsonl_summary(&jsonl_path).unwrap();
        assert_eq!(from_jsonl.nodes.len(), 2);

        let sum_path = dir.path().join("graph_summary.json");
        std::fs::write(&sum_path, serde_json::to_vec(&summary).unwrap()).unwrap();
        assert_eq!(
            inspect(dir.path(), Some("https://example.com/"), None, None, true).unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            inspect(&jsonl_path, None, None, None, false).unwrap(),
            ExitCode::SUCCESS
        );
    }
}
