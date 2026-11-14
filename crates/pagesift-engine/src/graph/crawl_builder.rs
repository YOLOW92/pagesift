use pagesift_api::CrawlSink;
use pagesift_api::types::Document;
use pagesift_graph::{
    DocNode, GraphOptions, GraphSummary, ProvenanceEdge, SourceKind, StreamingGraphBuilder,
};
use pagesift_store::Store;

use super::{PageGraphSeed, build_and_attach_page_graph, page_delta_from_document};

/// Coordinates streaming `PageGraphDelta` emission and final `GraphSummary` reconciliation across a crawl.
pub struct CrawlGraphCollector {
    options: GraphOptions,
    builder: StreamingGraphBuilder,
    store: Option<Store>,
    job_id: Option<String>,
    tenant: Option<String>,
}

impl CrawlGraphCollector {
    pub fn new(
        options: GraphOptions,
        store: Option<Store>,
        job_id: Option<String>,
        tenant: Option<String>,
    ) -> Self {
        let builder = StreamingGraphBuilder::new(options.clone());
        Self {
            options,
            builder,
            store,
            job_id,
            tenant,
        }
    }

    pub async fn ingest_doc(&mut self, doc: &mut Document, sink: &dyn CrawlSink) {
        if doc.graph.is_none() {
            let source_kind = doc
                .metadata
                .content_type
                .as_deref()
                .map(|ct| {
                    if ct.contains("pdf") {
                        SourceKind::Pdf
                    } else if ct.contains("json") {
                        SourceKind::Api
                    } else {
                        SourceKind::Html
                    }
                })
                .unwrap_or(SourceKind::Html);
            build_and_attach_page_graph(
                self.store.as_ref(),
                &self.options,
                doc,
                PageGraphSeed {
                    source_kind,
                    engine: "crawl".to_string(),
                    tenant: self.tenant.clone(),
                    ..Default::default()
                },
            );
        }

        let Some(slice) = doc.graph.clone() else {
            return;
        };
        let delta = page_delta_from_document(doc, &slice);
        let enriched = self.builder.ingest_delta(delta);
        if let Some(g) = doc.graph.as_mut()
            && enriched.version.is_some()
        {
            g.version = enriched.version.clone();
        }
        sink.graph_delta(enriched.clone()).await;
        if let (Some(st), Some(jid)) = (&self.store, &self.job_id) {
            let _ = st
                .graph_for(self.tenant.as_deref())
                .append_job_delta(jid, &enriched);
        }

        for member_delta in slice.member_deltas {
            let enriched_member = self.builder.ingest_delta(member_delta);
            sink.graph_delta(enriched_member.clone()).await;
            if let (Some(st), Some(jid)) = (&self.store, &self.job_id) {
                let _ = st
                    .graph_for(self.tenant.as_deref())
                    .append_job_delta(jid, &enriched_member);
            }
        }
    }

    /// Record redirect/duplicate provenance for a skipped duplicate URL without duplicating nodes or claims.
    pub fn ingest_provenance_only(&mut self, doc: &Document) {
        let Some(slice) = &doc.graph else {
            return;
        };
        self.builder.extend_provenance(slice.provenance.clone());
        if self.options.versions
            && let Some(st) = &self.store
        {
            let gs = st.graph_for(self.tenant.as_deref());
            for edge in &slice.provenance {
                match edge {
                    ProvenanceEdge::Redirect {
                        from_url, to_url, ..
                    }
                    | ProvenanceEdge::DuplicateOf {
                        source_url: from_url,
                        canonical_url: to_url,
                        ..
                    } => {
                        let from_k = DocNode::derive_resource_key(None, from_url);
                        let to_k = DocNode::derive_resource_key(None, to_url);
                        if from_k != to_k {
                            let _ = gs.bind_alias(&from_k, &to_k);
                        }
                    }
                    ProvenanceEdge::CanonicalizesTo {
                        source_url: from_url,
                        canonical_url: to_url,
                        cross_host,
                        ..
                    } => {
                        if *cross_host {
                            continue;
                        }
                        let from_k = DocNode::derive_resource_key(None, from_url);
                        let to_k = DocNode::derive_resource_key(None, to_url);
                        if from_k != to_k {
                            let _ = gs.bind_alias(&from_k, &to_k);
                        }
                    }
                    ProvenanceEdge::HasAlternate { .. }
                    | ProvenanceEdge::DerivedFrom { .. }
                    | ProvenanceEdge::AttachmentOf { .. } => {}
                }
            }
        }
    }

    pub async fn finish(self, sink: &dyn CrawlSink) -> GraphSummary {
        let summary = self.builder.into_summary();
        if let Some(st) = &self.store {
            let gs = st.graph_for(self.tenant.as_deref());
            if let Some(jid) = &self.job_id {
                if !summary.conflicts.is_empty() {
                    let _ = gs.put_conflicts(&summary.conflicts);
                }
                let _ = gs.put_job_summary(jid, &summary);
            } else if !summary.conflicts.is_empty() {
                // CLI 等无后台任务标识的场景同样需要写入租户内冲突索引；Put 按 conflict_id 幂等覆盖。
                let _ = gs.put_conflicts(&summary.conflicts);
            }
        }
        sink.graph_summary(summary.clone()).await;
        summary
    }
}
