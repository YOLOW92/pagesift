//! Graph summary finalization and cross-source fact/conflict reconciliation.

use std::collections::{BTreeMap, HashSet};

use super::StreamingGraphBuilder;
use crate::citation::CitationEdge;
use crate::conflict::{ConflictRecord, detect_conflicts_with_lineage};
use crate::corroborate::{ClaimRecord, FactCluster, build_fact_clusters_with_lineage};
use crate::delta::{GraphStats, GraphSummary};
use crate::independence::compute_source_lineage;
use crate::model::{DocNode, GraphOptions};
use crate::provenance::{CanonicalCluster, ProvenanceEdge, cluster_canonicals};

impl StreamingGraphBuilder {
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
    let lineage = if options.facts || options.conflicts {
        compute_source_lineage(nodes, provenance)
    } else {
        BTreeMap::new()
    };
    let fact_clusters = if options.facts {
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
        detect_conflicts_with_lineage(claims, &lineage, citations)
    } else {
        Vec::new()
    };
    (canonical_clusters, fact_clusters, conflicts)
}
