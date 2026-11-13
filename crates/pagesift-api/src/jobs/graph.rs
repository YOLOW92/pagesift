//! DataGraph persistence and query helpers for background crawl jobs and the global graph store.

use std::collections::{BTreeMap, BTreeSet};

use pagesift_graph::{
    ConflictRecord, DocNode, GraphOptions, GraphSummary, PageGraphDelta, StreamingGraphBuilder,
    VersionEdge, VersionRecord, normalize_resource_key,
};

use super::Jobs;
use super::state::JobState;
use crate::error::{ApiError, ErrorCode};

pub(super) async fn record_delta(jobs: &Jobs, id: &str, delta: PageGraphDelta) {
    {
        let mut map = jobs.inner.map.lock().await;
        let Some(job) = map.get_mut(id) else {
            return;
        };
        if job.state == JobState::Cancelled {
            return;
        }
        if jobs.inner.store.is_none() {
            job.graph_deltas.push(delta.clone());
        }
        job.touch();
    }
    if let Some(store) = &jobs.inner.store {
        let jid = id.to_string();
        if let Err(e) = store
            .blocking(move |s| {
                let g = s.graph();
                g.append_job_delta(&jid, &delta)?;
                g.record_version(&version_record(&delta.node), delta.version.as_ref())
            })
            .await
        {
            tracing::warn!(job = %id, error = %e, "persisting graph delta failed");
        }
    }
}

pub(super) async fn record_summary(jobs: &Jobs, id: &str, summary: GraphSummary) {
    if let Some(store) = &jobs.inner.store {
        let jid = id.to_string();
        let copy = summary.clone();
        if let Err(e) = store
            .blocking(move |s| {
                let g = s.graph();
                if !copy.conflicts.is_empty() {
                    g.put_conflicts(&copy.conflicts)?;
                }
                g.put_job_summary(&jid, &copy)
            })
            .await
        {
            tracing::warn!(job = %id, error = %e, "persisting graph summary failed");
        }
    }
    let mut map = jobs.inner.map.lock().await;
    let Some(job) = map.get_mut(id) else {
        return;
    };
    if job.state != JobState::Cancelled {
        job.graph_summary = Some(summary);
        job.touch();
    }
}

fn summarize_deltas(deltas: &[PageGraphDelta]) -> GraphSummary {
    if deltas.is_empty() {
        return GraphSummary::default();
    }
    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    for delta in deltas {
        builder.ingest_delta(delta.clone());
    }
    builder.finalize()
}

fn version_record(node: &DocNode) -> VersionRecord {
    VersionRecord {
        resource_key: node.resource_key.clone(),
        doc_id: node.doc_id.clone(),
        url: node.final_url.clone(),
        canonical_url: node.canonical_url.clone(),
        observed_at_ms: node.observed_at_ms,
        content_hash: node.content_hash.clone(),
        text_hash: node.text_hash.clone(),
        status_code: node.status_code,
        etag: node.etag.clone(),
        last_modified: node.last_modified.clone(),
    }
}

impl Jobs {
    /// Return the DataGraph summary for a crawl job, synthesizing from deltas if still in progress.
    pub async fn crawl_graph(&self, id: &str) -> Option<(JobState, GraphSummary)> {
        self.sweep().await;
        let (state, mem_summary, mem_deltas) = {
            let map = self.inner.map.lock().await;
            let job = map.get(id)?;
            (
                job.state,
                job.graph_summary.clone(),
                job.graph_deltas.clone(),
            )
        };
        if let Some(summary) = mem_summary {
            return Some((state, summary));
        }
        if let Some(store) = &self.inner.store {
            let jid = id.to_string();
            let stored = store
                .blocking(move |s| {
                    let g = s.graph();
                    if let Some(summary) = g.get_job_summary(&jid)? {
                        return Ok(Some(summary));
                    }
                    let deltas = g.job_deltas(&jid)?;
                    Ok::<_, pagesift_store::Error>(Some(summarize_deltas(&deltas)))
                })
                .await;
            if let Ok(Some(summary)) = stored {
                return Some((state, summary));
            }
        }
        Some((state, summarize_deltas(&mem_deltas)))
    }

    /// Query version lineage for a URL or resource key across the persistent or in-memory graph store.
    pub async fn graph_history(
        &self,
        url_or_key: &str,
    ) -> Result<(String, Vec<VersionRecord>, Vec<VersionEdge>), ApiError> {
        if let Some(store) = &self.inner.store {
            let target = url_or_key.to_string();
            return store
                .blocking(move |s| s.graph().history(&target))
                .await
                .map_err(|e| {
                    ApiError::new(ErrorCode::Internal, format!("graph history failed: {e}"))
                });
        }
        let target_key = if url_or_key.contains("://") {
            normalize_resource_key(url_or_key)
        } else {
            url_or_key.to_string()
        };
        let map = self.inner.map.lock().await;
        let mut versions_by_id: BTreeMap<String, VersionRecord> = BTreeMap::new();
        let mut edges_by_key: BTreeMap<(Option<String>, String), VersionEdge> = BTreeMap::new();
        let mut matching_docs = BTreeSet::new();
        for job in map.values() {
            for delta in &job.graph_deltas {
                if delta.node.resource_key == target_key {
                    matching_docs.insert(delta.node.doc_id.clone());
                    versions_by_id
                        .entry(delta.node.doc_id.clone())
                        .or_insert_with(|| version_record(&delta.node));
                    if let Some(edge) = &delta.version {
                        edges_by_key.insert(
                            (edge.prev_doc_id.clone(), edge.curr_doc_id.clone()),
                            edge.clone(),
                        );
                    }
                }
            }
            if let Some(summary) = &job.graph_summary {
                for node in &summary.nodes {
                    if node.resource_key == target_key {
                        matching_docs.insert(node.doc_id.clone());
                        versions_by_id
                            .entry(node.doc_id.clone())
                            .or_insert_with(|| version_record(node));
                    }
                }
                for edge in &summary.versions {
                    let prev_matches = edge
                        .prev_doc_id
                        .as_ref()
                        .is_some_and(|id| matching_docs.contains(id));
                    if prev_matches || matching_docs.contains(&edge.curr_doc_id) {
                        edges_by_key.insert(
                            (edge.prev_doc_id.clone(), edge.curr_doc_id.clone()),
                            edge.clone(),
                        );
                    }
                }
            }
        }
        let mut versions: Vec<VersionRecord> = versions_by_id.into_values().collect();
        versions.sort_by(|a, b| {
            a.observed_at_ms
                .cmp(&b.observed_at_ms)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        let edges: Vec<VersionEdge> = edges_by_key.into_values().collect();
        Ok((target_key, versions, edges))
    }

    /// List recorded DataGraph conflicts, optionally filtered by subject key.
    pub async fn graph_conflicts(
        &self,
        subject: Option<&str>,
    ) -> Result<Vec<ConflictRecord>, ApiError> {
        if let Some(store) = &self.inner.store {
            let subj = subject.map(str::to_string);
            return store
                .blocking(move |s| s.graph().list_conflicts(subj.as_deref()))
                .await
                .map_err(|e| {
                    ApiError::new(ErrorCode::Internal, format!("graph conflicts failed: {e}"))
                });
        }
        let map = self.inner.map.lock().await;
        let mut by_id: BTreeMap<String, ConflictRecord> = BTreeMap::new();
        for job in map.values() {
            let summary = if let Some(s) = &job.graph_summary {
                s.clone()
            } else if !job.graph_deltas.is_empty() {
                summarize_deltas(&job.graph_deltas)
            } else {
                continue;
            };
            for conflict in summary.conflicts {
                if subject.is_none_or(|s| conflict.subject_key == s) {
                    by_id
                        .entry(conflict.conflict_id.clone())
                        .or_insert(conflict);
                }
            }
        }
        Ok(by_id.into_values().collect())
    }
}
