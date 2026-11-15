use std::collections::{BTreeSet, HashMap};

use pagesift_api::types::Document;
use pagesift_graph::{
    CanonicalSource, DocNode, GraphOptions, GraphSlice, PageGraphDelta, ProvenanceEdge,
    RawFieldTrace, RedirectKind, SourceKind, build_indexed_blocks, canonical_edge,
};
use pagesift_store::Store;
use serde_json::Value;

use super::align::{BlockMapKind, build_old_to_new_block_map};
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
    tenant: Option<&str>,
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
        tenant: tenant.map(str::to_string),
        ..Default::default()
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
    tenant: Option<&str>,
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
    if opts.versions {
        let final_url = doc
            .metadata
            .url
            .as_deref()
            .or(doc.metadata.source_url.as_deref())
            .unwrap_or_default();
        let extras: Vec<String> = super::trusted_permanent_redirect_keys(redirect_chain, final_url)
            .into_iter()
            .filter(|k| *k != slice.resource_key)
            .collect();
        if opts.persist
            && let Some(st) = store
        {
            let gs = st.graph_for(tenant);
            for k in &extras {
                let _ = gs.bind_alias(k, &slice.resource_key);
            }
        } else if !opts.persist {
            super::stage::update_staged_aliases(&slice.doc_id, &extras);
        }
    }
}

/// Record duplicate provenance edge onto `doc.graph`.
pub fn enrich_duplicate_provenance(
    _store: Option<&Store>,
    _versions: bool,
    _tenant: Option<&str>,
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
            source_url,
            canonical_url: duplicate_of.to_string(),
            reason: reason.to_string(),
        };
        if !slice.provenance.contains(&edge) {
            slice.provenance.push(edge);
        }
    }
}

/// Synchronize derived `GraphSlice` blocks, evidence spans, and claims after `queryFilter`
/// and/or `chunking` transforms `doc.markdown` without overwriting original source provenance.
pub fn resync_graph_after_transform(
    doc: &mut Document,
    opts: &GraphOptions,
    pre_filter_md: Option<&str>,
) {
    let Some(slice) = doc.graph.as_mut() else {
        return;
    };
    let md = doc.markdown.as_deref().unwrap_or_default();
    let chunks = doc.chunks.as_deref();
    let spans = evidence::chunk_spans_of(chunks);
    let indexed = if md.is_empty() {
        Vec::new()
    } else {
        build_indexed_blocks(
            &slice.doc_id,
            md,
            &spans,
            opts.limits.max_blocks_per_doc,
            opts.limits.max_excerpt_chars,
        )
    };

    if let Some(old_md) = pre_filter_md {
        let old_indexed = if old_md.is_empty() {
            Vec::new()
        } else {
            build_indexed_blocks(
                &slice.doc_id,
                old_md,
                &[],
                opts.limits.max_blocks_per_doc,
                opts.limits.max_excerpt_chars,
            )
        };
        let old_to_new = build_old_to_new_block_map(&old_indexed, &indexed);
        slice.blocks = if opts.blocks {
            indexed.iter().map(|b| b.meta.clone()).collect()
        } else {
            Vec::new()
        };
        let claim_vals_by_field: HashMap<String, Value> = slice
            .claims
            .iter()
            .map(|c| (c.field_path.clone(), c.raw_value.clone()))
            .collect();
        for ev in &mut slice.evidence {
            let cval = claim_vals_by_field.get(&ev.field_path);
            super::align::realign_span_to_filtered_blocks(
                ev,
                cval,
                old_md,
                &old_indexed,
                &old_to_new,
                &indexed,
            );
        }
        for claim in &mut slice.claims {
            super::align::realign_span_to_filtered_blocks(
                &mut claim.evidence,
                Some(&claim.raw_value),
                old_md,
                &old_indexed,
                &old_to_new,
                &indexed,
            );
        }
        if let Some(ver) = slice.version.as_mut() {
            let mut remapped = Vec::new();
            let mut seen_ids = BTreeSet::new();
            for old_id in &ver.changed_block_ids {
                if let Some(&mapped) = old_to_new.get(old_id) {
                    let new_idx = match mapped {
                        BlockMapKind::Exact(i) | BlockMapKind::Partial(i) => i,
                    };
                    let new_bid = indexed[new_idx].meta.block_id.clone();
                    if seen_ids.insert(new_bid.clone()) {
                        remapped.push(new_bid);
                    }
                }
            }
            ver.changed_block_ids = remapped;
        }
        super::stage::update_staged_transform(&slice.doc_id, md, &spans);
        return;
    }

    slice.blocks = if opts.blocks {
        indexed.iter().map(|b| b.meta.clone()).collect()
    } else {
        Vec::new()
    };
    if let Some(chunks) = chunks {
        for ev in &mut slice.evidence {
            ev.chunk_idx = None;
            super::align::align_span_chunk(ev, chunks);
        }
        for claim in &mut slice.claims {
            claim.evidence.chunk_idx = None;
            super::align::align_span_chunk(&mut claim.evidence, chunks);
        }
    }
    super::stage::update_staged_transform(&slice.doc_id, md, &spans);
}

/// Update `GraphSlice.blocks` and `EvidenceSpan.chunk_idx` once chunking completes in `scrape_content`.
pub fn update_graph_from_chunks(doc: &mut Document, opts: &GraphOptions) {
    resync_graph_after_transform(doc, opts, None);
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
        temporal: slice.temporal.clone(),
        blocks: slice.blocks.clone(),
        citations: slice.citations.clone(),
        provenance: slice.provenance.clone(),
        version: slice.version.clone(),
        version_rejected: slice.version_rejected,
        evidence: slice.evidence.clone(),
        claims: slice.claims.clone(),
        claim_transitions: slice.claim_transitions.clone(),
    }
}
