//! Bounded streaming DataGraph builder that emits per-page deltas and reconciles cross-source facts.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::citation::{CitationEdge, aggregate_citations, extract_markdown_links};
use crate::conflict::detect_conflicts;
use crate::corroborate::{ClaimRecord, build_fact_clusters, resolve_subject_key};
use crate::delta::{DocumentInput, GraphStats, GraphSummary, PageGraphDelta};
use crate::evidence::{ExtractionMethod, RawFieldTrace, align_traces, collect_json_traces};
use crate::hash::{normalize_resource_key, normalize_url};
use crate::model::{BlockRef, DocNode, GraphOptions, SourceKind, build_indexed_blocks};
use crate::provenance::{CanonicalSource, ProvenanceEdge, canonical_edge, cluster_canonicals};
use crate::version::{VersionEdge, VersionRecord, link_version_pair};

/// Incrementally ingests scraped/parsed documents without retaining DOMs or full page text.
pub struct StreamingGraphBuilder {
    options: GraphOptions,
    nodes: Vec<DocNode>,
    crawled_resources: HashSet<String>,
    citations: Vec<CitationEdge>,
    provenance: Vec<ProvenanceEdge>,
    versions: Vec<VersionEdge>,
    latest_versions: BTreeMap<String, VersionRecord>,
    claims: Vec<ClaimRecord>,
    claim_bucket_counts: BTreeMap<(String, String), usize>,
    total_blocks: usize,
    total_evidence: usize,
}

impl StreamingGraphBuilder {
    pub fn new(options: GraphOptions) -> Self {
        Self {
            options,
            nodes: Vec::new(),
            crawled_resources: HashSet::new(),
            citations: Vec::new(),
            provenance: Vec::new(),
            versions: Vec::new(),
            latest_versions: BTreeMap::new(),
            claims: Vec::new(),
            claim_bucket_counts: BTreeMap::new(),
            total_blocks: 0,
            total_evidence: 0,
        }
    }

    /// Process one document, release transient full-block text immediately, and return a compact delta.
    pub fn ingest(&mut self, input: DocumentInput) -> PageGraphDelta {
        let final_url = normalize_url(&input.final_url);
        let canonical_url = input
            .canonical_url
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(normalize_url);
        let logical_url = canonical_url.as_deref().unwrap_or(&final_url);
        let resource_key = normalize_resource_key(logical_url);
        let doc_id = DocNode::compute_doc_id(
            logical_url,
            input.observed_at_ms,
            input.content_hash.as_deref().or(input.raw_hash.as_deref()),
        );

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

        let indexed_blocks = if self.options.blocks || self.options.evidence {
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

        let curr_record = VersionRecord {
            resource_key: resource_key.clone(),
            doc_id: doc_id.clone(),
            url: final_url.clone(),
            canonical_url: canonical_url.clone(),
            observed_at_ms: input.observed_at_ms,
            content_hash: input.content_hash.clone(),
            text_hash: input.text_hash.clone(),
            status_code: input.status_code,
            etag: input.etag.clone(),
            last_modified: input.last_modified.clone(),
        };
        let version = if self.options.versions {
            let prev = input
                .previous_version
                .as_ref()
                .or_else(|| self.latest_versions.get(&resource_key));
            prev.map(|p| {
                link_version_pair(
                    p,
                    &curr_record,
                    input.version_diff_stats.clone(),
                    input.changed_block_ids.clone(),
                )
            })
        } else {
            None
        };
        self.latest_versions
            .insert(resource_key.clone(), curr_record);

        let all_traces = collect_all_traces(&final_url, &input);
        let subject_key = resolve_subject_key(
            &self.options.subject_keys,
            &input.structured_json_ld,
            &all_traces,
            canonical_url.as_deref(),
            &resource_key,
        );

        let evidence = if self.options.evidence && !all_traces.is_empty() {
            align_traces(&doc_id, &final_url, &all_traces, &indexed_blocks)
        } else {
            Vec::new()
        };
        drop(indexed_blocks);

        let mut page_claims = Vec::new();
        if self.options.facts || self.options.conflicts {
            for (trace, span) in all_traces.iter().zip(evidence.iter()) {
                let claim = ClaimRecord::from_trace(
                    &subject_key.key,
                    &resource_key,
                    &doc_id,
                    &final_url,
                    input.source_kind.clone(),
                    &input.engine,
                    input.observed_at_ms,
                    canonical_url.clone(),
                    trace,
                    span.clone(),
                );
                let bucket = (claim.subject_key.clone(), claim.normalized_field.clone());
                let cnt = self.claim_bucket_counts.entry(bucket).or_insert(0);
                if *cnt < self.options.limits.max_claims_per_bucket {
                    *cnt += 1;
                    self.claims.push(claim.clone());
                    page_claims.push(claim);
                }
            }
        }

        let kept_blocks = if self.options.blocks {
            blocks
        } else {
            Vec::new()
        };
        self.total_blocks += kept_blocks.len();
        self.total_evidence += evidence.len();
        self.crawled_resources.insert(resource_key);
        self.nodes.push(node.clone());
        self.citations.extend(citations.clone());
        self.provenance.extend(provenance.clone());
        if let Some(ref v) = version {
            self.versions.push(v.clone());
        }

        PageGraphDelta {
            node,
            subject_key,
            blocks: kept_blocks,
            citations,
            provenance,
            version,
            evidence,
            claims: page_claims,
        }
    }

    /// Ingest an already-constructed [`PageGraphDelta`] into the crawl/job summary without re-parsing text.
    pub fn ingest_delta(&mut self, delta: PageGraphDelta) -> PageGraphDelta {
        let resource_key = delta.node.resource_key.clone();
        self.total_blocks += delta.blocks.len();
        self.total_evidence += delta.evidence.len();
        self.crawled_resources.insert(resource_key.clone());
        self.nodes.push(delta.node.clone());
        self.citations.extend(delta.citations.clone());
        self.provenance.extend(delta.provenance.clone());
        let mut version = delta.version.clone();
        if self.options.versions {
            let curr_record = VersionRecord {
                resource_key: resource_key.clone(),
                doc_id: delta.node.doc_id.clone(),
                url: delta.node.final_url.clone(),
                canonical_url: delta.node.canonical_url.clone(),
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
                let linked = link_version_pair(prev, &curr_record, None, Vec::new());
                version = Some(linked);
            }
            if let Some(ref v) = version {
                self.versions.push(v.clone());
            }
            self.latest_versions.insert(resource_key, curr_record);
        }
        for claim in &delta.claims {
            let bucket = (claim.subject_key.clone(), claim.normalized_field.clone());
            let cnt = self.claim_bucket_counts.entry(bucket).or_insert(0);
            if *cnt < self.options.limits.max_claims_per_bucket {
                *cnt += 1;
                self.claims.push(claim.clone());
            }
        }
        PageGraphDelta { version, ..delta }
    }

    /// Record additional provenance edges (e.g. from skipped duplicate redirects) without duplicating document nodes or claims.
    pub fn extend_provenance(&mut self, edges: impl IntoIterator<Item = ProvenanceEdge>) {
        for edge in edges {
            if !self.provenance.contains(&edge) {
                self.provenance.push(edge);
            }
        }
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
        let canonical_clusters = cluster_canonicals(&self.provenance);
        let fact_clusters = if self.options.facts {
            build_fact_clusters(&self.claims, self.options.limits.max_claims_per_bucket)
        } else {
            Vec::new()
        };
        let conflicts = if self.options.conflicts {
            detect_conflicts(&self.claims)
        } else {
            Vec::new()
        };

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
            },
            nodes: self.nodes.clone(),
            canonical_clusters,
            citations,
            provenance: self.provenance.clone(),
            versions: self.versions.clone(),
            fact_clusters,
            conflicts,
        }
    }
}

fn build_provenance(node: &DocNode, input: &DocumentInput) -> Vec<ProvenanceEdge> {
    let mut edges = Vec::new();
    for (from_url, to_url, status, kind) in &input.redirect_chain {
        edges.push(ProvenanceEdge::Redirect {
            from_url: normalize_url(from_url),
            to_url: normalize_url(to_url),
            status: *status,
            redirect_kind: *kind,
        });
    }
    if let Some(can) = &node.canonical_url {
        let declared_by = if input.canonical_from_header {
            CanonicalSource::HttpHeader
        } else {
            CanonicalSource::HtmlLink
        };
        if let Some(edge) = canonical_edge(&node.final_url, can, declared_by) {
            edges.push(edge);
        }
    }
    for (alt_url, hreflang, media_type) in &input.alternates {
        edges.push(ProvenanceEdge::HasAlternate {
            primary_url: node.final_url.clone(),
            alt_url: normalize_url(alt_url),
            media_type: media_type.clone(),
            hreflang: hreflang.clone(),
        });
    }
    if let Some(dup) = &input.duplicate_of {
        edges.push(ProvenanceEdge::DuplicateOf {
            source_url: node.final_url.clone(),
            canonical_url: normalize_url(dup),
            reason: "content_duplicate".into(),
        });
    }
    match &node.source_kind {
        SourceKind::ArchiveMember {
            archive_url,
            member_path,
        } => {
            edges.push(ProvenanceEdge::AttachmentOf {
                attachment_url: node.final_url.clone(),
                parent_url: normalize_url(archive_url),
                parent_doc_id: None,
                media_id: None,
                member_path: Some(member_path.clone()),
            });
        }
        SourceKind::EmailPart {
            mail_url,
            part_name,
        } => {
            edges.push(ProvenanceEdge::AttachmentOf {
                attachment_url: node.final_url.clone(),
                parent_url: normalize_url(mail_url),
                parent_doc_id: None,
                media_id: None,
                member_path: Some(part_name.clone()),
            });
        }
        SourceKind::ImageAsset {
            parent_url,
            media_id,
        } => {
            edges.push(ProvenanceEdge::AttachmentOf {
                attachment_url: node.final_url.clone(),
                parent_url: normalize_url(parent_url),
                parent_doc_id: None,
                media_id: Some(media_id.clone()),
                member_path: None,
            });
        }
        _ => {}
    }
    for (att_url, media_id, member_path) in &input.attachments {
        edges.push(ProvenanceEdge::AttachmentOf {
            attachment_url: normalize_url(att_url),
            parent_url: node.final_url.clone(),
            parent_doc_id: Some(node.doc_id.clone()),
            media_id: media_id.clone(),
            member_path: member_path.clone(),
        });
    }
    edges
}

fn collect_all_traces(final_url: &str, input: &DocumentInput) -> Vec<RawFieldTrace> {
    let mut traces = input.raw_traces.clone();
    let mut seen: HashSet<(String, String)> = traces
        .iter()
        .map(|t| (t.field_path.clone(), t.raw_quote.clone()))
        .collect();

    let mut add_extra = Vec::new();
    for item in &input.structured_json_ld {
        let obj = item.as_object();
        let entity_type = obj
            .and_then(|m| m.get("@type"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let entity_id = obj
            .and_then(|m| m.get("@id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let method = ExtractionMethod::JsonLd {
            entity_type,
            entity_id,
        };
        collect_json_traces(item, "", &method, false, Some(final_url), &mut add_extra);
    }
    if let Some(val) = &input.extracted_fields {
        let method = ExtractionMethod::CssSelector {
            selector: "schema".into(),
            relocated_from: None,
            step: None,
        };
        collect_json_traces(val, "", &method, true, Some(final_url), &mut add_extra);
    }

    for trace in add_extra {
        if seen.insert((trace.field_path.clone(), trace.raw_quote.clone())) {
            traces.push(trace);
        }
    }
    traces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedirectKind;
    use serde_json::json;

    #[test]
    fn streams_multi_source_corporate_graph_end_to_end() {
        let mut builder = StreamingGraphBuilder::new(GraphOptions {
            subject_keys: vec!["sku".into()],
            ..GraphOptions::enabled()
        });
        let html_delta = builder.ingest(DocumentInput {
            requested_url: "https://shop.example.com/pro".into(),
            final_url: "https://example.com/pro".into(),
            canonical_url: Some("https://example.com/pro".into()),
            canonical_from_header: false,
            source_kind: SourceKind::Html,
            observed_at_ms: 1_000,
            status_code: 200,
            engine: "http".into(),
            content_hash: Some("h1".into()),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("Pro Plan".into()),
            markdown: "# Pro Plan\n\nPro tier costs $49/mo. See [Enterprise PDF](/docs/pro.pdf).\n"
                .into(),
            redirect_chain: vec![(
                "https://shop.example.com/pro".into(),
                "https://example.com/pro".into(),
                301,
                RedirectKind::HttpPermanent,
            )],
            links: Vec::new(),
            alternates: Vec::new(),
            duplicate_of: None,
            attachments: vec![("https://example.com/docs/pro.pdf".into(), None, None)],
            structured_json_ld: vec![
                json!({"@type": "Product", "sku": "PRO-1", "price": "$49/mo"}),
            ],
            extracted_fields: None,
            raw_traces: Vec::new(),
            chunk_spans: vec![(0, 0, 72)],
            previous_version: None,
            version_diff_stats: None,
            changed_block_ids: Vec::new(),
        });
        assert_eq!(html_delta.evidence[0].confidence, 0.95);
        assert!(html_delta.evidence[0].verified);

        builder.ingest(DocumentInput {
            requested_url: "https://example.com/docs/pro.pdf".into(),
            final_url: "https://example.com/docs/pro.pdf".into(),
            canonical_url: None,
            canonical_from_header: false,
            source_kind: SourceKind::Pdf,
            observed_at_ms: 2_000,
            status_code: 200,
            engine: "upload".into(),
            content_hash: Some("h2".into()),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("Pro Spec PDF".into()),
            markdown: "Pro plan (SKU PRO-1) is billed at $59/mo as of 2026-11.\n".into(),
            redirect_chain: Vec::new(),
            links: Vec::new(),
            alternates: Vec::new(),
            duplicate_of: None,
            attachments: Vec::new(),
            structured_json_ld: Vec::new(),
            extracted_fields: Some(json!({"sku": "PRO-1", "price": "$59/mo"})),
            raw_traces: Vec::new(),
            chunk_spans: Vec::new(),
            previous_version: None,
            version_diff_stats: None,
            changed_block_ids: Vec::new(),
        });

        let summary = builder.finalize();
        assert_eq!(summary.stats.nodes, 2);
        assert!(summary.citations.iter().any(|c| c.crawled));
        assert_eq!(summary.stats.conflicts, 1);
        assert_eq!(summary.conflicts[0].variants.len(), 2);
    }
}
