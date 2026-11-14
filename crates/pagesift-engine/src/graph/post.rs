use std::collections::{BTreeSet, HashMap};

use pagesift_api::types::{DocChunk, Document};
use pagesift_graph::{
    CanonicalSource, DocNode, EvidenceAlignment, EvidenceSpan, GraphOptions, GraphSlice,
    IndexedBlock, PageGraphDelta, ProvenanceEdge, RawFieldTrace, RedirectKind, SourceKind,
    build_indexed_blocks, canonical_edge,
};
use pagesift_store::Store;
use serde_json::Value;

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
    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph_for(tenant);
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
    versions: bool,
    tenant: Option<&str>,
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
    if versions && let Some(st) = store {
        let src_k = DocNode::derive_resource_key(None, &source_url);
        let dst_k = DocNode::derive_resource_key(None, duplicate_of);
        if src_k != dst_k {
            let _ = st.graph_for(tenant).bind_alias(&src_k, &dst_k);
        }
    }
}

fn align_span_chunk(span: &mut EvidenceSpan, chunks: &[DocChunk]) {
    if let (Some(start), Some(end)) = (span.start, span.end)
        && let Some((idx, _)) = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| start < c.end && end > c.start)
            .max_by_key(|(_, c)| end.min(c.end).saturating_sub(start.max(c.start)))
    {
        span.chunk_idx = Some(idx);
    }
}

fn norm_match_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn value_text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn find_matching_block<'a>(
    candidates: &[String],
    indexed: &'a [IndexedBlock],
) -> Option<&'a IndexedBlock> {
    for raw in candidates {
        let needle = norm_match_text(raw);
        if needle.is_empty() {
            continue;
        }
        if let Some(b) = indexed.iter().find(|b| norm_match_text(&b.text) == needle) {
            return Some(b);
        }
    }
    const MIN_REALIGN_LEN: usize = 4; // [策略] 最短有效过滤重对齐片段字符数
    for raw in candidates {
        let needle = norm_match_text(raw);
        if needle.len() < MIN_REALIGN_LEN {
            continue;
        }
        if let Some(b) = indexed.iter().find(|b| {
            let bt = norm_match_text(&b.text);
            bt.contains(&needle) || (bt.len() >= 8 && needle.contains(&bt)) // [策略] 短块反包含最小字符数
        }) {
            return Some(b);
        }
    }
    None
}

fn realign_span_to_filtered_blocks(
    span: &mut EvidenceSpan,
    claim_val: Option<&Value>,
    old_md: &str,
    old_block_texts: &HashMap<String, String>,
    indexed: &[IndexedBlock],
) {
    let had_body_coords = span.block_id.is_some() || span.start.is_some();
    if !had_body_coords && span.method.is_metadata_island() {
        // MetadataIsland 原先就没有正文字段坐标：允许借过滤后正文重对齐，否则维持 MetadataIsland  provenance。
        if let Some(val_str) = value_text(claim_val)
            && let Some(hit) = find_matching_block(&[val_str], indexed)
        {
            span.block_id = Some(hit.meta.block_id.clone());
            span.chunk_idx = hit.meta.chunk_idx;
            span.start = Some(hit.meta.start);
            span.end = Some(hit.meta.end);
            span.excerpt = hit.meta.excerpt.clone();
            span.confidence = 0.95; // [策略] 元数据岛在过滤后正文重命中的高置信度
            span.verified = true;
        }
        return;
    }

    let mut needles = Vec::new();
    if let Some(bid) = span.block_id.as_deref()
        && let Some(old_txt) = old_block_texts.get(bid)
    {
        needles.push(old_txt.clone());
    }
    if let (Some(s), Some(e)) = (span.start, span.end)
        && s <= e
        && e <= old_md.len()
        && old_md.is_char_boundary(s)
        && old_md.is_char_boundary(e)
    {
        needles.push(old_md[s..e].to_string());
    }
    if let Some(val_str) = value_text(claim_val) {
        needles.push(val_str);
    }
    if !span.excerpt.trim().is_empty() {
        needles.push(span.excerpt.clone());
    }

    if let Some(hit) = find_matching_block(&needles, indexed) {
        span.block_id = Some(hit.meta.block_id.clone());
        span.chunk_idx = hit.meta.chunk_idx;
        span.start = Some(hit.meta.start);
        span.end = Some(hit.meta.end);
        span.excerpt = hit.meta.excerpt.clone();
    } else {
        span.block_id = None;
        span.chunk_idx = None;
        span.start = None;
        span.end = None;
        span.verified = false;
        if span.method.is_metadata_island() {
            span.alignment = EvidenceAlignment::MetadataIsland;
            span.confidence = 0.85; // [策略] 元数据岛引用内容被查询过滤移除后的保留置信度
        } else {
            span.alignment = EvidenceAlignment::Unaligned;
            span.confidence = 0.0;
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
        let mut old_block_texts = HashMap::new();
        for b in &slice.blocks {
            if b.start <= b.end
                && b.end <= old_md.len()
                && old_md.is_char_boundary(b.start)
                && old_md.is_char_boundary(b.end)
            {
                old_block_texts.insert(b.block_id.clone(), old_md[b.start..b.end].to_string());
            }
        }
        slice.blocks = if opts.blocks {
            indexed.iter().map(|b| b.meta.clone()).collect()
        } else {
            Vec::new()
        };
        for ev in &mut slice.evidence {
            realign_span_to_filtered_blocks(ev, None, old_md, &old_block_texts, &indexed);
        }
        for claim in &mut slice.claims {
            realign_span_to_filtered_blocks(
                &mut claim.evidence,
                Some(&claim.raw_value),
                old_md,
                &old_block_texts,
                &indexed,
            );
        }
        if let Some(ver) = slice.version.as_mut() {
            let mut remapped = Vec::new();
            let mut seen_ids = BTreeSet::new();
            for old_id in &ver.changed_block_ids {
                if let Some(old_txt) = old_block_texts.get(old_id)
                    && let Some(hit) = find_matching_block(std::slice::from_ref(old_txt), &indexed)
                    && seen_ids.insert(hit.meta.block_id.clone())
                {
                    remapped.push(hit.meta.block_id.clone());
                }
            }
            ver.changed_block_ids = remapped;
        }
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
            align_span_chunk(ev, chunks);
        }
        for claim in &mut slice.claims {
            claim.evidence.chunk_idx = None;
            align_span_chunk(&mut claim.evidence, chunks);
        }
    }
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
        blocks: slice.blocks.clone(),
        citations: slice.citations.clone(),
        provenance: slice.provenance.clone(),
        version: slice.version.clone(),
        evidence: slice.evidence.clone(),
        claims: slice.claims.clone(),
    }
}
