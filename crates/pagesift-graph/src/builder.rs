//! Bounded streaming DataGraph builder that emits per-page deltas and reconciles cross-source facts.

use std::collections::{BTreeMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::citation::{CitationEdge, aggregate_citations, extract_markdown_links};
use crate::conflict::{ConflictRecord, detect_conflicts};
use crate::corroborate::{
    ClaimRecord, FactCluster, build_fact_clusters_with_lineage, resolve_subject_key,
};
use crate::delta::{DocumentInput, GraphStats, GraphSummary, PageGraphDelta};
use crate::evidence::align_traces;
use crate::evidence_json::collect_all_traces;
use crate::hash::{normalize_resource_key, normalize_url};
use crate::independence::compute_source_lineage;
use crate::lifecycle::{
    ClaimLifecycleRecord, ClaimTransition, ObservationOutcome, reconcile_resource_claims,
};
use crate::model::{BlockRef, DocNode, GraphOptions, build_indexed_blocks};
use crate::provenance::{CanonicalCluster, ProvenanceEdge, build_provenance, cluster_canonicals};
use crate::temporal::TemporalSemantics;
use crate::version::{VersionEdge, VersionRecord, link_version_pair};

/// Incrementally ingests scraped/parsed documents without retaining DOMs or full page text.
pub struct StreamingGraphBuilder {
    options: GraphOptions,
    nodes: Vec<DocNode>,
    crawled_resources: HashSet<String>,
    citations: Vec<CitationEdge>,
    provenance: Vec<ProvenanceEdge>,
    provenance_hashes: HashSet<u64>,
    versions: Vec<VersionEdge>,
    latest_versions: BTreeMap<String, VersionRecord>,
    claims: Vec<ClaimRecord>,
    claim_bucket_counts: BTreeMap<(String, String), usize>,
    resource_claim_lifecycles: BTreeMap<String, Vec<ClaimLifecycleRecord>>,
    claim_transitions: Vec<ClaimTransition>,
    total_blocks: usize,
    total_evidence: usize,
    truncated: bool,
    truncation_reasons: Vec<String>,
}

impl StreamingGraphBuilder {
    pub fn new(mut options: GraphOptions) -> Self {
        options.limits = options.limits.clamped();
        Self {
            options,
            nodes: Vec::new(),
            crawled_resources: HashSet::new(),
            citations: Vec::new(),
            provenance: Vec::new(),
            provenance_hashes: HashSet::new(),
            versions: Vec::new(),
            latest_versions: BTreeMap::new(),
            claims: Vec::new(),
            claim_bucket_counts: BTreeMap::new(),
            resource_claim_lifecycles: BTreeMap::new(),
            claim_transitions: Vec::new(),
            total_blocks: 0,
            total_evidence: 0,
            truncated: false,
            truncation_reasons: Vec::new(),
        }
    }

    fn mark_truncated(&mut self, reason: &'static str) {
        self.truncated = true;
        if !self.truncation_reasons.iter().any(|r| r == reason) {
            self.truncation_reasons.push(reason.to_owned());
        }
    }

    fn push_node_bounded(&mut self, node: DocNode, resource_key: &str) {
        let crawled_cap = self.options.limits.max_nodes.saturating_mul(2); // [策略] 已抓取键追踪集合按节点预算两倍设上限
        if self.crawled_resources.len() < crawled_cap
            || self.crawled_resources.contains(resource_key)
        {
            self.crawled_resources.insert(resource_key.to_owned());
        }
        if self.nodes.len() < self.options.limits.max_nodes {
            self.nodes.push(node);
        } else {
            self.mark_truncated("max_nodes");
        }
    }

    fn push_citations_bounded(&mut self, citations: &[CitationEdge]) {
        if !self.options.citations || citations.is_empty() {
            return;
        }
        let avail = self
            .options
            .limits
            .max_citations
            .saturating_sub(self.citations.len());
        if citations.len() > avail {
            self.mark_truncated("max_citations");
        }
        self.citations.extend(citations.iter().take(avail).cloned());
    }

    fn push_provenance_edge(&mut self, edge: ProvenanceEdge) {
        if !self.options.provenance {
            return;
        }
        let hash = provenance_hash(&edge);
        if self.provenance_hashes.contains(&hash) && self.provenance.contains(&edge) {
            return;
        }
        if self.provenance.len() >= self.options.limits.max_provenance_edges {
            self.mark_truncated("max_provenance_edges");
            return;
        }
        self.provenance_hashes.insert(hash);
        self.provenance.push(edge);
    }

    fn push_version_bounded(&mut self, edge: VersionEdge) {
        if self.versions.len() < self.options.limits.max_versions {
            self.versions.push(edge);
        } else {
            self.mark_truncated("max_versions");
        }
    }

    fn record_latest_version(&mut self, resource_key: String, record: VersionRecord) {
        let cap = self.options.limits.max_nodes.saturating_mul(2); // [策略] 最新版本游标索引按节点预算两倍设上限
        if self.latest_versions.contains_key(&resource_key) || self.latest_versions.len() < cap {
            self.latest_versions.insert(resource_key, record);
        } else {
            self.mark_truncated("max_versions");
        }
    }

    fn push_claim_bounded(&mut self, claim: &ClaimRecord) -> bool {
        let bucket = (claim.subject_key.clone(), claim.normalized_field.clone());
        if !self.claim_bucket_counts.contains_key(&bucket)
            && self.claim_bucket_counts.len() >= self.options.limits.max_claim_buckets
        {
            self.mark_truncated("max_claim_buckets");
            return false;
        }
        if self.claims.len() >= self.options.limits.max_claims {
            self.mark_truncated("max_claims");
            return false;
        }
        let cnt = self.claim_bucket_counts.entry(bucket).or_insert(0);
        if *cnt >= self.options.limits.max_claims_per_bucket {
            self.mark_truncated("max_claims_per_bucket");
            return false;
        }
        *cnt += 1;
        self.claims.push(claim.clone());
        true
    }

    /// Process one document, release transient full-block text immediately, and return a compact delta.
    pub fn ingest(&mut self, input: DocumentInput) -> PageGraphDelta {
        let final_url = normalize_url(&input.final_url);
        let canonical_url = input
            .canonical_url
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(normalize_url);
        let trusted_canonical =
            DocNode::trusted_canonical_url(canonical_url.as_deref(), &final_url);
        let logical_url = trusted_canonical.as_deref().unwrap_or(&final_url);
        let resource_key = normalize_resource_key(logical_url);
        let doc_id = DocNode::compute_doc_id(
            logical_url,
            input.observed_at_ms,
            input.content_hash.as_deref().or(input.raw_hash.as_deref()),
        );
        let temporal = TemporalSemantics::from_document_sources(
            input.observed_at_ms,
            None,
            None,
            input.last_modified.as_deref(),
            &input.structured_json_ld,
        );
        let has_rich_temporal = temporal.published_at_ms.is_some()
            || temporal.modified_at_ms.is_some()
            || temporal.valid_from.is_some()
            || temporal.valid_until.is_some();

        let node = DocNode {
            doc_id: doc_id.clone(),
            resource_key: resource_key.clone(),
            requested_url: normalize_url(&input.requested_url),
            final_url: final_url.clone(),
            canonical_url: canonical_url.clone(),
            source_kind: input.source_kind.clone(),
            observed_at_ms: input.observed_at_ms,
            status_code: input.status_code,
            engine: input.engine.clone(),
            content_hash: input.content_hash.clone(),
            raw_hash: input.raw_hash.clone(),
            text_hash: input.text_hash.clone(),
            etag: input.etag.clone(),
            last_modified: input.last_modified.clone(),
            title: input.title.clone(),
        };

        let need_alignment = self.options.evidence || self.options.facts || self.options.conflicts;
        let need_blocks = self.options.blocks
            || need_alignment
            || (self.options.citations && input.links.is_empty() && !input.markdown.is_empty());
        let indexed_blocks = if need_blocks {
            build_indexed_blocks(
                &doc_id,
                &input.markdown,
                &input.chunk_spans,
                self.options.limits.max_blocks_per_doc,
                self.options.limits.max_excerpt_chars,
            )
        } else {
            Vec::new()
        };
        let blocks: Vec<BlockRef> = indexed_blocks.iter().map(|b| b.meta.clone()).collect();

        let citations = if self.options.citations {
            let mut raw_links = input.links.clone();
            if raw_links.is_empty() && !input.markdown.is_empty() {
                raw_links = extract_markdown_links(&input.markdown, Some(&final_url), &blocks);
            }
            aggregate_citations(
                &doc_id,
                &final_url,
                &resource_key,
                &raw_links,
                self.options.limits.max_edges_per_doc,
                self.options.limits.max_anchor_samples,
            )
        } else {
            Vec::new()
        };
        let provenance = if self.options.provenance {
            build_provenance(&node, &input)
        } else {
            Vec::new()
        };

        let version = if self.options.versions {
            let curr_record = VersionRecord {
                resource_key: resource_key.clone(),
                doc_id: doc_id.clone(),
                url: final_url.clone(),
                canonical_url: trusted_canonical.clone(),
                observed_at_ms: input.observed_at_ms,
                content_hash: input.content_hash.clone(),
                text_hash: input.text_hash.clone(),
                status_code: input.status_code,
                etag: input.etag.clone(),
                last_modified: input.last_modified.clone(),
            };
            let prev = input
                .previous_version
                .as_ref()
                .or_else(|| self.latest_versions.get(&resource_key));
            let edge = prev.map(|p| {
                link_version_pair(
                    p,
                    &curr_record,
                    input.version_diff_stats.clone(),
                    input.changed_block_ids.clone(),
                )
            });
            self.record_latest_version(resource_key.clone(), curr_record);
            edge
        } else {
            None
        };

        let all_traces = collect_all_traces(&final_url, &input);
        let subject_key = resolve_subject_key(
            &self.options.subject_keys,
            &input.structured_json_ld,
            &all_traces,
            trusted_canonical.as_deref(),
            &resource_key,
        );
        let aligned_evidence = if need_alignment && !all_traces.is_empty() {
            align_traces(&doc_id, &final_url, &all_traces, &indexed_blocks)
        } else {
            Vec::new()
        };
        drop(indexed_blocks);

        let mut page_claims = Vec::new();
        if self.options.facts || self.options.conflicts {
            for (trace, span) in all_traces.iter().zip(aligned_evidence.iter()) {
                let mut claim = ClaimRecord::from_trace(
                    &subject_key.key,
                    &resource_key,
                    &doc_id,
                    &final_url,
                    input.source_kind.clone(),
                    &input.engine,
                    input.observed_at_ms,
                    trusted_canonical.clone(),
                    trace,
                    span.clone(),
                );
                if has_rich_temporal {
                    claim.temporal = Some(temporal.clone());
                }
                if self.push_claim_bounded(&claim) {
                    page_claims.push(claim);
                }
            }
        }
        let claim_transitions = self.reconcile_page_claim_lifecycles(
            &resource_key,
            input.status_code,
            input.observed_at_ms,
            &page_claims,
        );
        let evidence = if self.options.evidence {
            aligned_evidence
        } else {
            Vec::new()
        };
        let kept_blocks = if self.options.blocks {
            blocks
        } else {
            Vec::new()
        };
        self.total_blocks += kept_blocks.len();
        self.total_evidence += evidence.len();
        self.push_node_bounded(node.clone(), &resource_key);
        self.push_citations_bounded(&citations);
        for edge in &provenance {
            self.push_provenance_edge(edge.clone());
        }
        if let Some(ref v) = version {
            self.push_version_bounded(v.clone());
        }

        PageGraphDelta {
            node,
            subject_key,
            temporal: has_rich_temporal.then_some(temporal),
            blocks: kept_blocks,
            citations,
            provenance,
            version,
            version_rejected: false,
            evidence,
            claims: page_claims,
            claim_transitions,
        }
    }

    fn reconcile_page_claim_lifecycles(
        &mut self,
        resource_key: &str,
        status_code: u16,
        observed_at_ms: u64,
        page_claims: &[ClaimRecord],
    ) -> Vec<ClaimTransition> {
        if !(self.options.facts || self.options.conflicts) {
            return Vec::new();
        }
        let outcome = outcome_for_status(status_code);
        let had_prev = self.resource_claim_lifecycles.contains_key(resource_key);
        let prev = self
            .resource_claim_lifecycles
            .get(resource_key)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let (updated, transitions) =
            reconcile_resource_claims(prev, &outcome, page_claims, observed_at_ms);
        let cap = self.options.limits.max_nodes.saturating_mul(2);
        if had_prev || self.resource_claim_lifecycles.len() < cap {
            self.resource_claim_lifecycles
                .insert(resource_key.to_owned(), updated);
        }
        if had_prev {
            self.claim_transitions.extend(transitions.clone());
            transitions
        } else {
            Vec::new()
        }
    }

    /// Ingest an already-constructed [`PageGraphDelta`] into the crawl/job summary without re-parsing text.
    pub fn ingest_delta(&mut self, mut delta: PageGraphDelta) -> PageGraphDelta {
        let resource_key = delta.node.resource_key.clone();
        if !self.options.blocks {
            delta.blocks.clear();
        }
        if !self.options.citations {
            delta.citations.clear();
        }
        if !self.options.provenance {
            delta.provenance.clear();
        }
        if !self.options.evidence {
            delta.evidence.clear();
        }
        self.total_blocks += delta.blocks.len();
        self.total_evidence += delta.evidence.len();
        self.push_node_bounded(delta.node.clone(), &resource_key);
        self.push_citations_bounded(&delta.citations);
        for edge in &delta.provenance {
            self.push_provenance_edge(edge.clone());
        }

        let mut version = if self.options.versions && !delta.version_rejected {
            delta.version.clone()
        } else {
            None
        };
        if self.options.versions && !delta.version_rejected {
            let trusted_canonical = DocNode::trusted_canonical_url(
                delta.node.canonical_url.as_deref(),
                &delta.node.final_url,
            );
            let curr_record = VersionRecord {
                resource_key: resource_key.clone(),
                doc_id: delta.node.doc_id.clone(),
                url: delta.node.final_url.clone(),
                canonical_url: trusted_canonical,
                observed_at_ms: delta.node.observed_at_ms,
                content_hash: delta.node.content_hash.clone(),
                text_hash: delta.node.text_hash.clone(),
                status_code: delta.node.status_code,
                etag: delta.node.etag.clone(),
                last_modified: delta.node.last_modified.clone(),
            };
            if version.is_none()
                && let Some(prev) = self.latest_versions.get(&resource_key)
                && prev.doc_id != curr_record.doc_id
            {
                version = Some(link_version_pair(prev, &curr_record, None, Vec::new()));
            }
            if let Some(ref v) = version {
                self.push_version_bounded(v.clone());
            }
            self.record_latest_version(resource_key.clone(), curr_record);
        }

        if self.options.facts || self.options.conflicts {
            let mut kept_claims = Vec::with_capacity(delta.claims.len());
            for claim in &delta.claims {
                if self.push_claim_bounded(claim) {
                    kept_claims.push(claim.clone());
                }
            }
            delta.claims = kept_claims;
            let transitions = self.reconcile_page_claim_lifecycles(
                &resource_key,
                delta.node.status_code,
                delta.node.observed_at_ms,
                &delta.claims,
            );
            if delta.claim_transitions.is_empty() {
                delta.claim_transitions = transitions;
            } else {
                self.claim_transitions
                    .extend(delta.claim_transitions.clone());
            }
        } else {
            delta.claims.clear();
            delta.claim_transitions.clear();
        }
        PageGraphDelta { version, ..delta }
    }

    /// Record additional provenance edges without duplicating document nodes or claims.
    pub fn extend_provenance(&mut self, edges: impl IntoIterator<Item = ProvenanceEdge>) {
        for edge in edges {
            self.push_provenance_edge(edge);
        }
    }

    /// Reconcile crawled status, corroborated fact clusters, and conflicts without cloning buffers.
    pub fn into_summary(mut self) -> GraphSummary {
        let summary = self.finalize();
        self.crawled_resources = HashSet::new();
        self.latest_versions = BTreeMap::new();
        self.claim_bucket_counts = BTreeMap::new();
        self.provenance_hashes = HashSet::new();
        self.resource_claim_lifecycles = BTreeMap::new();
        self.claims = Vec::new();
        summary
    }

    /// Reconcile crawled status, corroborated fact clusters, and conflicts across all ingested documents.
    pub fn finalize(&self) -> GraphSummary {
        let citations: Vec<CitationEdge> = self
            .citations
            .iter()
            .cloned()
            .map(|mut edge| {
                edge.crawled = self.crawled_resources.contains(&edge.to_resource_key);
                edge
            })
            .collect();
        let (canonical_clusters, fact_clusters, conflicts) = summarize_vectors(
            &self.options,
            &self.nodes,
            &self.provenance,
            &citations,
            &self.claims,
        );

        GraphSummary {
            stats: GraphStats {
                nodes: self.nodes.len(),
                blocks: self.total_blocks,
                citations: citations.len(),
                provenance_edges: self.provenance.len(),
                version_edges: self.versions.len(),
                evidence_spans: self.total_evidence,
                claims: self.claims.len(),
                fact_clusters: fact_clusters.len(),
                conflicts: conflicts.len(),
                canonical_clusters: canonical_clusters.len(),
                truncated: self.truncated,
                truncation_reasons: self.truncation_reasons.clone(),
            },
            nodes: self.nodes.clone(),
            canonical_clusters,
            citations,
            provenance: self.provenance.clone(),
            versions: self.versions.clone(),
            fact_clusters,
            conflicts,
            claim_transitions: self.claim_transitions.clone(),
        }
    }
}

fn outcome_for_status(status_code: u16) -> ObservationOutcome {
    match status_code {
        304 => ObservationOutcome::NotModified304,
        404 => ObservationOutcome::SuspectedGone {
            status_code: Some(404),
        },
        410 => ObservationOutcome::ConfirmedGone {
            reason: "HTTP 410 Gone".into(),
        },
        429 | 500..=599 => ObservationOutcome::TransientFetchError {
            error: format!("HTTP {status_code}"),
        },
        _ => ObservationOutcome::VerifiedContent,
    }
}

fn provenance_hash(edge: &ProvenanceEdge) -> u64 {
    let mut hasher = DefaultHasher::new();
    edge.hash(&mut hasher);
    hasher.finish()
}

fn summarize_vectors(
    options: &GraphOptions,
    nodes: &[DocNode],
    provenance: &[ProvenanceEdge],
    citations: &[CitationEdge],
    claims: &[ClaimRecord],
) -> (Vec<CanonicalCluster>, Vec<FactCluster>, Vec<ConflictRecord>) {
    let canonical_clusters = if options.provenance {
        cluster_canonicals(provenance)
    } else {
        Vec::new()
    };
    let fact_clusters = if options.facts {
        let lineage = compute_source_lineage(nodes, provenance);
        build_fact_clusters_with_lineage(
            claims,
            options.limits.max_claims_per_bucket,
            &lineage,
            citations,
        )
    } else {
        Vec::new()
    };
    let conflicts = if options.conflicts {
        detect_conflicts(claims)
    } else {
        Vec::new()
    };
    (canonical_clusters, fact_clusters, conflicts)
}
