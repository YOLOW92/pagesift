//! Non-speculative DataGraph staging for `Scraper::scrape` and `Sync` ticks (C01 / C02 / C06).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use pagesift_api::types::Document;
use pagesift_graph::{DocNode, GraphSlice, ObservationOutcome, VersionRecord};

tokio::task_local! {
    static GRAPH_COMMIT_STAGE: Arc<Mutex<HashMap<String, StagedGraphCommit>>>;
}

/// Captured pre-persistence graph payload held in memory until the winning `Document` is chosen.
#[derive(Debug, Clone)]
pub struct StagedGraphCommit {
    pub doc_id: String,
    pub resource_key: String,
    pub node: DocNode,
    pub alias_keys: Vec<String>,
    pub markdown: Option<String>,
    pub matched_304_md: Option<String>,
    pub not_modified: bool,
    pub not_modified_unavail_reason: Option<String>,
    pub previous_version: Option<VersionRecord>,
    pub matched_304_version: Option<VersionRecord>,
    pub chunk_spans: Vec<(usize, usize, usize)>,
    pub had_initial_prev: bool,
    pub seed_content_hash: Option<String>,
    pub seed_raw_hash: Option<String>,
}

/// Run an async operation inside a per-scrape staging scope; speculative attempts never hit disk.
pub async fn with_graph_stage<F, T>(fut: F) -> (T, HashMap<String, StagedGraphCommit>)
where
    F: Future<Output = T>,
{
    let map = Arc::new(Mutex::new(HashMap::new()));
    let res = GRAPH_COMMIT_STAGE.scope(Arc::clone(&map), fut).await;
    let entries = map
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    (res, entries)
}

/// Return true if execution is inside an active deferred-commit staging scope.
pub(crate) fn is_stage_active() -> bool {
    GRAPH_COMMIT_STAGE.try_with(|_| ()).is_ok()
}

pub(crate) fn push_staged_commit(entry: StagedGraphCommit) {
    let _ = GRAPH_COMMIT_STAGE.try_with(|m| {
        if let Ok(mut guard) = m.lock() {
            guard.insert(entry.doc_id.clone(), entry);
        }
    });
}

pub(crate) fn update_staged_aliases(doc_id: &str, extra_aliases: &[String]) {
    if extra_aliases.is_empty() {
        return;
    }
    let _ = GRAPH_COMMIT_STAGE.try_with(|m| {
        if let Ok(mut guard) = m.lock()
            && let Some(entry) = guard.get_mut(doc_id)
        {
            for a in extra_aliases {
                if *a != entry.resource_key && !entry.alias_keys.contains(a) {
                    entry.alias_keys.push(a.clone());
                }
            }
        }
    });
}

pub(crate) fn update_staged_transform(
    doc_id: &str,
    markdown: &str,
    chunk_spans: &[(usize, usize, usize)],
) {
    let _ = GRAPH_COMMIT_STAGE.try_with(|m| {
        if let Ok(mut guard) = m.lock()
            && let Some(entry) = guard.get_mut(doc_id)
        {
            entry.markdown = Some(markdown.to_string());
            entry.chunk_spans = chunk_spans.to_vec();
        }
    });
}

/// Classify a fetched `Document` into a clean graph `ObservationOutcome` (C02).
pub fn observation_outcome_for_doc(doc: &Document, slice: &GraphSlice) -> ObservationOutcome {
    let ps = doc.metadata.pagesift.as_ref();
    if ps.is_some_and(|p| p.not_modified) {
        return ObservationOutcome::NotModified304;
    }
    if let Some(p) = ps
        && p.warnings.iter().any(|w| {
            w.starts_with("barrier:")
                || w.contains("access_restricted")
                || w.contains("bot_challenge")
        })
    {
        return ObservationOutcome::TransientFetchError {
            error: "barrier detected".to_string(),
        };
    }
    match doc.metadata.status_code {
        404 => ObservationOutcome::SuspectedGone {
            status_code: Some(404),
        },
        410 => ObservationOutcome::ConfirmedGone {
            reason: "HTTP 410 Gone".to_string(),
        },
        s if s >= 400 => ObservationOutcome::TransientFetchError {
            error: format!("HTTP {s}"),
        },
        _ if slice.source_kind == pagesift_graph::SourceKind::Html
            && doc.metadata.status_code == 200
            && doc.markdown.as_deref().is_none_or(|m| m.trim().is_empty())
            && doc
                .extract
                .as_ref()
                .is_none_or(|e| e.as_array().is_some_and(Vec::is_empty))
            && slice.claims.is_empty()
            && doc.metadata.pagesift.as_ref().is_some_and(|p| {
                p.warnings
                    .iter()
                    .any(|w| w.contains("render_timeout") || w.contains("render_failed"))
            }) =>
        {
            ObservationOutcome::TransientFetchError {
                error: "empty shell after failed render".to_string(),
            }
        }
        _ => ObservationOutcome::VerifiedContent,
    }
}

pub fn is_authoritative_outcome(outcome: &ObservationOutcome) -> bool {
    matches!(
        outcome,
        ObservationOutcome::VerifiedContent | ObservationOutcome::NotModified304
    )
}
