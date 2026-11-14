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
    let (tenant, versions_enabled) = {
        let mut map = jobs.inner.map.lock().await;
        let Some(job) = map.get_mut(id) else {
            return;
        };
        if job.state == JobState::Cancelled {
            return;
        }
        let tenant = job.tenant.clone();
        let versions_enabled = job.graph_versions;
        if jobs.inner.store.is_none() {
            job.graph_deltas.push(delta.clone());
        }
        job.touch();
        (tenant, versions_enabled)
    };
    if let Some(store) = &jobs.inner.store {
        let jid = id.to_string();
        if let Err(e) = store
            .blocking(move |s| {
                let g = s.graph_for(tenant.as_deref());
                g.append_job_delta(&jid, &delta)?;
                if versions_enabled {
                    g.record_version(&version_record(&delta.node), delta.version.as_ref())?;
                }
                Ok::<_, pagesift_store::Error>(())
            })
            .await
        {
            tracing::warn!(job = %id, error = %e, "persisting graph delta failed");
        }
    }
}

pub(super) async fn record_summary(jobs: &Jobs, id: &str, summary: GraphSummary) {
    let tenant = {
        let map = jobs.inner.map.lock().await;
        let Some(job) = map.get(id) else {
            return;
        };
        if job.state == JobState::Cancelled {
            return;
        }
        job.tenant.clone()
    };
    if let Some(store) = &jobs.inner.store {
        let jid = id.to_string();
        let copy = summary.clone();
        if let Err(e) = store
            .blocking(move |s| {
                let g = s.graph_for(tenant.as_deref());
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

fn summarize_deltas(deltas: &[PageGraphDelta], opts: &GraphOptions) -> GraphSummary {
    if deltas.is_empty() {
        return GraphSummary::default();
    }
    let mut builder = StreamingGraphBuilder::new(opts.clone());
    for delta in deltas {
        builder.ingest_delta(delta.clone());
    }
    builder.into_summary()
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
    /// Return the DataGraph summary for a crawl job, synthesizing from bounded deltas if in progress.
    pub async fn crawl_graph(
        &self,
        id: &str,
    ) -> Result<Option<(JobState, GraphSummary)>, ApiError> {
        self.sweep().await;
        let (state, tenant, graph_opts, mem_summary, mem_deltas) = {
            let map = self.inner.map.lock().await;
            let Some(job) = map.get(id) else {
                return Ok(None);
            };
            (
                job.state,
                job.tenant.clone(),
                job.graph_options.clone(),
                job.graph_summary.clone(),
                job.graph_deltas.clone(),
            )
        };
        if let Some(summary) = mem_summary {
            return Ok(Some((state, summary)));
        }
        if let Some(store) = &self.inner.store {
            let jid = id.to_string();
            let opts = graph_opts.clone();
            let summary = store
                .blocking(move |s| {
                    let g = s.graph_for(tenant.as_deref());
                    if let Some(summary) = g.get_job_summary(&jid)? {
                        return Ok(summary);
                    }
                    let mut builder = StreamingGraphBuilder::new(opts);
                    let truncated = g.visit_job_deltas(
                        &jid,
                        pagesift_store::DEFAULT_GRAPH_DELTAS_LIMIT,
                        |delta| {
                            builder.ingest_delta(delta);
                            Ok(())
                        },
                    )?;
                    let mut out = builder.into_summary();
                    if truncated {
                        out.stats.truncated = true;
                        out.stats
                            .truncation_reasons
                            .push("job_deltas_limit_reached".into());
                    }
                    Ok::<_, pagesift_store::Error>(out)
                })
                .await
                .map_err(|e| {
                    ApiError::new(ErrorCode::Internal, format!("crawl graph failed: {e}"))
                })?;
            return Ok(Some((state, summary)));
        }
        Ok(Some((state, summarize_deltas(&mem_deltas, &graph_opts))))
    }

    /// Query version lineage for a URL or resource key for one isolated tenant (or local `None`).
    pub async fn graph_history_for(
        &self,
        tenant: Option<&str>,
        url_or_key: &str,
        limit: usize,
    ) -> Result<pagesift_store::GraphHistoryResult, ApiError> {
        if let Some(store) = &self.inner.store {
            let target = url_or_key.to_string();
            let scope = tenant.map(str::to_string);
            return store
                .blocking(move |s| {
                    s.graph_for(scope.as_deref())
                        .history_bounded(&target, limit)
                })
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
        for job in map.values().filter(|j| j.tenant.as_deref() == tenant) {
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
        let mut edges: Vec<VersionEdge> = edges_by_key.into_values().collect();
        edges.sort_by(|a, b| {
            a.curr_observed_at_ms
                .cmp(&b.curr_observed_at_ms)
                .then_with(|| a.curr_doc_id.cmp(&b.curr_doc_id))
        });
        let mut truncated = false;
        if versions.len() > limit {
            let skip = versions.len() - limit;
            versions.drain(..skip);
            truncated = true;
        }
        if edges.len() > limit {
            let skip = edges.len() - limit;
            edges.drain(..skip);
            truncated = true;
        }
        Ok(pagesift_store::GraphHistoryResult {
            resource_key: target_key,
            versions,
            edges,
            truncated,
        })
    }

    /// Query bounded local (unscoped) version lineage for embedded/MCP callers.
    pub async fn graph_history(
        &self,
        url_or_key: &str,
    ) -> Result<(String, Vec<VersionRecord>, Vec<VersionEdge>), ApiError> {
        let res = self
            .graph_history_for(
                None,
                url_or_key,
                pagesift_store::DEFAULT_GRAPH_HISTORY_LIMIT,
            )
            .await?;
        Ok((res.resource_key, res.versions, res.edges))
    }

    /// List paginated DataGraph conflicts for one isolated tenant (or local `None`).
    pub async fn graph_conflicts_for(
        &self,
        tenant: Option<&str>,
        subject: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(usize, Vec<ConflictRecord>, bool), ApiError> {
        if let Some(store) = &self.inner.store {
            let subj = subject.map(str::to_string);
            let scope = tenant.map(str::to_string);
            let (items, total, has_more) = store
                .blocking(move |s| {
                    s.graph_for(scope.as_deref()).list_conflicts_bounded(
                        subj.as_deref(),
                        limit,
                        offset,
                    )
                })
                .await
                .map_err(|e| {
                    ApiError::new(ErrorCode::Internal, format!("graph conflicts failed: {e}"))
                })?;
            return Ok((total, items, has_more));
        }
        let map = self.inner.map.lock().await;
        let mut by_id: BTreeMap<String, ConflictRecord> = BTreeMap::new();
        for job in map.values().filter(|j| j.tenant.as_deref() == tenant) {
            let summary = if let Some(s) = &job.graph_summary {
                s.clone()
            } else if !job.graph_deltas.is_empty() {
                summarize_deltas(&job.graph_deltas, &job.graph_options)
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
        let total = by_id.len();
        if offset >= total || limit == 0 {
            return Ok((total, Vec::new(), offset < total));
        }
        let end = total.min(offset.saturating_add(limit));
        let has_more = end < total;
        let page: Vec<ConflictRecord> = by_id
            .into_values()
            .skip(offset)
            .take(end - offset)
            .collect();
        Ok((total, page, has_more))
    }

    /// List bounded local (unscoped) DataGraph conflicts for embedded/MCP callers.
    pub async fn graph_conflicts(
        &self,
        subject: Option<&str>,
    ) -> Result<Vec<ConflictRecord>, ApiError> {
        let (_, page, _) = self
            .graph_conflicts_for(
                None,
                subject,
                pagesift_store::DEFAULT_GRAPH_CONFLICTS_LIMIT,
                0,
            )
            .await?;
        Ok(page)
    }
}
