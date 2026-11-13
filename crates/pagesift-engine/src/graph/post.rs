use pagesift_api::types::{DocChunk, Document};
use pagesift_graph::{
    CanonicalSource, DocNode, EvidenceSpan, GraphOptions, GraphSlice, PageGraphDelta,
    ProvenanceEdge, RawFieldTrace, RedirectKind, SourceKind, build_block_refs, canonical_edge,
};
use pagesift_store::Store;

use super::dom_links::DomGraphData;
use super::{PageGraphSeed, build_and_attach_page_graph, evidence};

/// Merge HTTP `Link: rel="alternate"` headers into `Document.alternates` without duplicating entries.
pub fn merge_header_alternates(doc: &mut Document, header_alternates: &[(String, String)]) {
    for pair in header_alternates {
        if !doc.alternates.contains(pair) {
            doc.alternates.push(pair.clone());
        }
    }
}

/// Build and attach a `GraphSlice` for HTML documents (HTTP fetch, browser render, or HTML upload).
#[allow(clippy::too_many_arguments)]
pub fn attach_html_graph(
    store: Option<&Store>,
    opts: &GraphOptions,
    doc: &mut Document,
    dom_data: DomGraphData,
    raw_traces: Vec<RawFieldTrace>,
    redirect_chain: Vec<(String, String, u16, RedirectKind)>,
    canonical_from_header: bool,
    header_alternates: &[(String, String)],
    engine: &str,
    markdown_fallback: Option<String>,
) {
    if doc.metadata.canonical_url.is_none() {
        doc.metadata.canonical_url = dom_data.canonical_url;
    }
    merge_header_alternates(doc, header_alternates);
    let restore_none = doc.markdown.is_none();
    if restore_none {
        doc.markdown = markdown_fallback;
    }
    let ps = doc.metadata.pagesift.as_ref();
    let seed = PageGraphSeed {
        source_kind: SourceKind::Html,
        engine: engine.to_string(),
        canonical_from_header,
        content_hash: ps
            .and_then(|p| p.content_hash.clone())
            .or(dom_data.markdown_hash),
        raw_hash: ps
            .and_then(|p| p.raw_hash.clone())
            .or(dom_data.raw_html_hash),
        text_hash: ps.and_then(|p| p.text_hash.clone()).or(dom_data.text_hash),
        etag: ps.and_then(|p| p.etag.clone()),
        last_modified: ps.and_then(|p| p.last_modified.clone()),
        redirect_chain,
        links: dom_data.links,
        alternates: doc
            .alternates
            .iter()
            .map(|(lang, url)| {
                (
                    url.clone(),
                    lang.contains('/').then(|| lang.clone()),
                    (!lang.contains('/') && !lang.trim().is_empty() && lang != "alternate")
                        .then(|| lang.clone()),
                )
            })
            .collect(),
        attachments: doc
            .images
            .as_ref()
            .map(|imgs| imgs.iter().map(|u| (u.clone(), None, None)).collect())
            .unwrap_or_default(),
        json_ld: dom_data.json_ld,
        raw_traces,
        member_deltas: Vec::new(),
    };
    build_and_attach_page_graph(store, opts, doc, seed);
    if restore_none {
        doc.markdown = None;
    }
}

/// Apply HTTP-level redirects, HTTP `Link: rel="canonical"` source attribution, and alternates
/// onto a `Document` produced by a format-specific extractor (PDF, Office, Image, JSON).
pub fn apply_http_provenance(
    store: Option<&Store>,
    opts: &GraphOptions,
    doc: &mut Document,
    redirect_chain: &[(String, String, u16, RedirectKind)],
    canonical_from_header: bool,
    header_alternates: &[(String, String)],
) {
    let Some(slice) = doc.graph.as_mut() else {
        return;
    };
    for (from_u, to_u, status, kind) in redirect_chain {
        let edge = ProvenanceEdge::Redirect {
            from_url: from_u.clone(),
            to_url: to_u.clone(),
            status: *status,
            redirect_kind: *kind,
        };
        if !slice.provenance.contains(&edge) {
            slice.provenance.push(edge);
        }
    }
    if canonical_from_header && let Some(canon) = doc.metadata.canonical_url.as_deref() {
        let source_url = doc
            .metadata
            .url
            .as_deref()
            .or(doc.metadata.source_url.as_deref())
            .unwrap_or_default();
        if !source_url.is_empty()
            && let Some(header_edge) =
                canonical_edge(source_url, canon, CanonicalSource::HttpHeader)
        {
            slice.provenance.retain(|e| {
                !matches!(
                    e,
                    ProvenanceEdge::CanonicalizesTo {
                        source_url: s,
                        canonical_url: c,
                        declared_by: CanonicalSource::HtmlLink,
                        ..
                    } if s == source_url && c == canon
                )
            });
            if !slice.provenance.contains(&header_edge) {
                slice.provenance.push(header_edge);
            }
        }
    }
    if let Some(primary) = doc.metadata.url.as_deref() {
        for (label, alt_url) in header_alternates {
            let edge = ProvenanceEdge::HasAlternate {
                primary_url: primary.to_string(),
                alt_url: alt_url.clone(),
                media_type: label.contains('/').then(|| label.clone()),
                hreflang: (!label.contains('/') && label != "alternate").then(|| label.clone()),
            };
            if !slice.provenance.contains(&edge) {
                slice.provenance.push(edge);
            }
        }
    }
    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph();
        for (from_u, to_u, _, _) in redirect_chain {
            for u in [from_u, to_u] {
                let k = DocNode::derive_resource_key(None, u);
                if k != slice.resource_key {
                    let _ = gs.bind_alias(&k, &slice.resource_key);
                }
            }
        }
    }
}

/// Record duplicate provenance edge onto `doc.graph` and alias into `Store`.
pub fn enrich_duplicate_provenance(
    store: Option<&Store>,
    doc: &mut Document,
    duplicate_of: &str,
    reason: &str,
) {
    let source_url = doc
        .metadata
        .url
        .clone()
        .or_else(|| doc.metadata.source_url.clone())
        .unwrap_or_default();
    if source_url.is_empty() {
        return;
    }
    if let Some(slice) = doc.graph.as_mut() {
        let edge = ProvenanceEdge::DuplicateOf {
            source_url: source_url.clone(),
            canonical_url: duplicate_of.to_string(),
            reason: reason.to_string(),
        };
        if !slice.provenance.contains(&edge) {
            slice.provenance.push(edge);
        }
    }
    if let Some(st) = store {
        let src_k = DocNode::derive_resource_key(None, &source_url);
        let dst_k = DocNode::derive_resource_key(None, duplicate_of);
        if src_k != dst_k {
            let _ = st.graph().bind_alias(&src_k, &dst_k);
        }
    }
}

fn align_span_chunk(span: &mut EvidenceSpan, chunks: &[DocChunk]) {
    if let (Some(start), Some(end)) = (span.start, span.end)
        && let Some((idx, _)) = chunks
            .iter()
            .enumerate()
            .find(|(_, c)| start < c.end && end > c.start)
    {
        span.chunk_idx = Some(idx);
    }
}

/// Update `GraphSlice.blocks` and `EvidenceSpan.chunk_idx` once chunking completes in `scrape_content`.
pub fn update_graph_from_chunks(doc: &mut Document, opts: &GraphOptions) {
    let (Some(slice), Some(chunks)) = (doc.graph.as_mut(), doc.chunks.as_deref()) else {
        return;
    };
    let md = doc.markdown.as_deref().unwrap_or_default();
    let spans = evidence::chunk_spans_of(Some(chunks));
    if opts.blocks && !md.is_empty() {
        slice.blocks = build_block_refs(
            &slice.doc_id,
            md,
            &spans,
            opts.limits.max_blocks_per_doc,
            opts.limits.max_excerpt_chars,
        );
    }
    for ev in &mut slice.evidence {
        align_span_chunk(ev, chunks);
    }
    for claim in &mut slice.claims {
        align_span_chunk(&mut claim.evidence, chunks);
    }
}

/// Reconstruct a streamable `PageGraphDelta` from a `Document` and its attached `GraphSlice`.
pub fn page_delta_from_document(doc: &Document, slice: &GraphSlice) -> PageGraphDelta {
    let final_url = doc
        .metadata
        .url
        .clone()
        .or_else(|| doc.metadata.source_url.clone())
        .unwrap_or_else(|| slice.resource_key.clone());
    let requested_url = doc
        .metadata
        .source_url
        .clone()
        .unwrap_or_else(|| final_url.clone());
    let ps = doc.metadata.pagesift.as_ref();
    let engine = ps
        .and_then(|p| p.engine.clone())
        .unwrap_or_else(|| slice.source_kind.format_label());

    PageGraphDelta {
        node: DocNode {
            doc_id: slice.doc_id.clone(),
            resource_key: slice.resource_key.clone(),
            requested_url,
            final_url,
            canonical_url: doc.metadata.canonical_url.clone(),
            source_kind: slice.source_kind.clone(),
            observed_at_ms: slice.observed_at_ms,
            status_code: doc.metadata.status_code,
            engine,
            content_hash: ps.and_then(|p| p.content_hash.clone()),
            raw_hash: ps.and_then(|p| p.raw_hash.clone()),
            text_hash: ps.and_then(|p| p.text_hash.clone()),
            etag: ps.and_then(|p| p.etag.clone()),
            last_modified: ps.and_then(|p| p.last_modified.clone()),
            title: doc.metadata.title.clone(),
        },
        subject_key: slice.subject_key.clone(),
        blocks: slice.blocks.clone(),
        citations: slice.citations.clone(),
        provenance: slice.provenance.clone(),
        version: slice.version.clone(),
        evidence: slice.evidence.clone(),
        claims: slice.claims.clone(),
    }
}
