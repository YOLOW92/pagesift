//! Bounded streaming DataGraph builder that emits per-page deltas and reconciles cross-source facts.

use std::collections::{BTreeMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::citation::{CitationEdge, aggregate_citations_checked, extract_markdown_links};
use crate::conflict::detect_conflicts_with_lineage;
use crate::corroborate::{ClaimRecord, resolve_entity_subject_keys};
use crate::delta::{DocumentInput, PageGraphDelta};
use crate::evidence::align_traces;
use crate::evidence_json::collect_all_traces;
use crate::hash::{normalize_resource_key, normalize_url};
use crate::lifecycle::{
    ClaimLifecycleRecord, ClaimTransition, ObservationOutcome, outcome_for_scrape,
    reconcile_resource_claims,
};
use crate::model::{BlockRef, DocNode, GraphOptions, build_indexed_blocks_checked};
use crate::provenance::{ProvenanceEdge, build_provenance};
use crate::temporal::TemporalSemantics;
use crate::version::{VersionEdge, VersionRecord, link_version_pair};

#[path = "builder_summary.rs"]
mod builder_summary;

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

    fn note_doc_truncation(&mut self, doc_reasons: &mut Vec<String>, reason: &'static str) {
        self.mark_truncated(reason);
        if !doc_reasons.iter().any(|r| r == reason) {
            doc_reasons.push(reason.to_owned());
        }
    }

    fn push_node_bounded(
        &mut self,
        node: DocNode,
        resource_key: &str,
        doc_reasons: &mut Vec<String>,
    ) {
        let crawled_cap = self.options.limits.max_nodes.saturating_mul(2); // [策略] 已抓取键追踪集合按节点预算两倍设上限
        if self.crawled_resources.len() < crawled_cap
            || self.crawled_resources.contains(resource_key)
        {
            self.crawled_resources.insert(resource_key.to_owned());
        }
        if self.nodes.len() < self.options.limits.max_nodes {
            self.nodes.push(node);
        } else {
            self.note_doc_truncation(doc_reasons, "max_nodes");
        }
    }

    fn push_citations_bounded(
        &mut self,
        citations: &[CitationEdge],
        doc_reasons: &mut Vec<String>,
    ) {
        if !self.options.citations || citations.is_empty() {
            return;
        }
        let avail = self
            .options
            .limits
            .max_citations
            .saturating_sub(self.citations.len());
        if citations.len() > avail {
            self.note_doc_truncation(doc_reasons, "max_citations");
        }
        self.citations.extend(citations.iter().take(avail).cloned());
    }

    fn push_provenance_edge(&mut self, edge: ProvenanceEdge) -> Option<&'static str> {
        if !self.options.provenance {
            return None;
        }
        let hash = provenance_hash(&edge);
        if self.provenance_hashes.contains(&hash) && self.provenance.contains(&edge) {
            return None;
        }
        if self.provenance.len() >= self.options.limits.max_provenance_edges {
            self.mark_truncated("max_provenance_edges");
            return Some("max_provenance_edges");
        }
        self.provenance_hashes.insert(hash);
        self.provenance.push(edge);
        None
    }

    fn push_version_bounded(&mut self, edge: VersionEdge, doc_reasons: &mut Vec<String>) {
        if self.versions.len() < self.options.limits.max_versions {
            self.versions.push(edge);
        } else {
            self.note_doc_truncation(doc_reasons, "max_versions");
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

    fn push_claim_bounded(&mut self, claim: &ClaimRecord, doc_reasons: &mut Vec<String>) -> bool {
        let bucket = (claim.subject_key.clone(), claim.normalized_field.clone());
        if !self.claim_bucket_counts.contains_key(&bucket)
            && self.claim_bucket_counts.len() >= self.options.limits.max_claim_buckets
        {
            self.note_doc_truncation(doc_reasons, "max_claim_buckets");
            return false;
        }
        if self.claims.len() >= self.options.limits.max_claims {
            self.note_doc_truncation(doc_reasons, "max_claims");
            return false;
        }
        let cnt = self.claim_bucket_counts.entry(bucket).or_insert(0);
        if *cnt >= self.options.limits.max_claims_per_bucket {
            self.note_doc_truncation(doc_reasons, "max_claims_per_bucket");
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
        let has_rich_temporal = temporal.has_rich_metadata();

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

        let mut doc_reasons = Vec::new();
        let need_alignment = self.options.evidence || self.options.facts || self.options.conflicts;
        let need_blocks = self.options.blocks
            || need_alignment
            || (self.options.citations && input.links.is_empty() && !input.markdown.is_empty());
        let (indexed_blocks, blocks_truncated) = if need_blocks {
            build_indexed_blocks_checked(
                &doc_id,
                &input.markdown,
                &input.chunk_spans,
                self.options.limits.max_blocks_per_doc,
                self.options.limits.max_excerpt_chars,
            )
        } else {
            (Vec::new(), false)
        };
        if blocks_truncated {
            self.note_doc_truncation(&mut doc_reasons, "max_blocks_per_doc");
        }
        let blocks: Vec<BlockRef> = indexed_blocks.iter().map(|b| b.meta.clone()).collect();

        let (citations, citations_truncated) = if self.options.citations {
            let mut raw_links = input.links.clone();
            if raw_links.is_empty() && !input.markdown.is_empty() {
                raw_links = extract_markdown_links(&input.markdown, Some(&final_url), &blocks);
            }
            aggregate_citations_checked(
                &doc_id,
                &final_url,
                &resource_key,
                &raw_links,
                self.options.limits.max_edges_per_doc,
                self.options.limits.max_anchor_samples,
            )
        } else {
            (Vec::new(), false)
        };
        if citations_truncated {
            self.note_doc_truncation(&mut doc_reasons, "max_edges_per_doc");
        }
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
        let (subject_key, trace_subjects) = resolve_entity_subject_keys(
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
            for ((trace, trace_subject), span) in all_traces
                .iter()
                .zip(trace_subjects.iter())
                .zip(aligned_evidence.iter())
            {
                let mut claim = ClaimRecord::from_trace(
                    &trace_subject.key,
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
                let claim_temporal = TemporalSemantics::for_claim(
                    &temporal,
                    &input.structured_json_ld,
                    &trace.field_name,
                    &trace.value,
                );
                if claim_temporal.has_rich_metadata() {
                    claim.temporal = Some(claim_temporal);
                }
                if self.push_claim_bounded(&claim, &mut doc_reasons) {
                    page_claims.push(claim);
                }
            }
        }
        let page_conflicts = if self.options.conflicts && !page_claims.is_empty() {
            detect_conflicts_with_lineage(&page_claims, &BTreeMap::new(), &[])
        } else {
            Vec::new()
        };
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
        self.push_node_bounded(node.clone(), &resource_key, &mut doc_reasons);
        self.push_citations_bounded(&citations, &mut doc_reasons);
        for edge in &provenance {
            if let Some(reason) = self.push_provenance_edge(edge.clone())
                && !doc_reasons.iter().any(|r| r == reason)
            {
                doc_reasons.push(reason.to_owned());
            }
        }
        if let Some(ref v) = version {
            self.push_version_bounded(v.clone(), &mut doc_reasons);
        }

        let truncated = !doc_reasons.is_empty();
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
            conflicts: page_conflicts,
            truncated,
            truncation_reasons: doc_reasons,
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
        let mut doc_reasons = std::mem::take(&mut delta.truncation_reasons);
        if delta.truncated {
            self.truncated = true;
            for r in &doc_reasons {
                if !self.truncation_reasons.iter().any(|x| x == r) {
                    self.truncation_reasons.push(r.clone());
                }
            }
        }
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
        self.push_node_bounded(delta.node.clone(), &resource_key, &mut doc_reasons);
        self.push_citations_bounded(&delta.citations, &mut doc_reasons);
        for edge in &delta.provenance {
            if let Some(reason) = self.push_provenance_edge(edge.clone())
                && !doc_reasons.iter().any(|r| r == reason)
            {
                doc_reasons.push(reason.to_owned());
            }
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
                self.push_version_bounded(v.clone(), &mut doc_reasons);
            }
            self.record_latest_version(resource_key.clone(), curr_record);
        }

        if self.options.facts || self.options.conflicts {
            let mut kept_claims = Vec::with_capacity(delta.claims.len());
            for claim in &delta.claims {
                if self.push_claim_bounded(claim, &mut doc_reasons) {
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
        if !self.options.conflicts {
            delta.conflicts.clear();
        } else if delta.conflicts.is_empty() && !delta.claims.is_empty() {
            delta.conflicts = detect_conflicts_with_lineage(&delta.claims, &BTreeMap::new(), &[]);
        }
        let truncated = delta.truncated || !doc_reasons.is_empty();
        PageGraphDelta {
            version,
            truncated,
            truncation_reasons: doc_reasons,
            ..delta
        }
    }

    /// Record additional provenance edges without duplicating document nodes or claims.
    pub fn extend_provenance(&mut self, edges: impl IntoIterator<Item = ProvenanceEdge>) {
        for edge in edges {
            self.push_provenance_edge(edge);
        }
    }
}

fn outcome_for_status(status_code: u16) -> ObservationOutcome {
    outcome_for_scrape(status_code, false, true, false, false, None)
}

fn provenance_hash(edge: &ProvenanceEdge) -> u64 {
    let mut hasher = DefaultHasher::new();
    edge.hash(&mut hasher);
    hasher.finish()
}
