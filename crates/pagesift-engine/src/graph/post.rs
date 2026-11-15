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
    if opts.versions
        && let Some(st) = store
    {
        let final_url = doc
            .metadata
            .url
            .as_deref()
            .or(doc.metadata.source_url.as_deref())
            .unwrap_or_default();
        let gs = st.graph_for(tenant);
        for k in super::trusted_permanent_redirect_keys(redirect_chain, final_url) {
            if k != slice.resource_key {
                let _ = gs.bind_alias(&k, &slice.resource_key);
            }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockMapKind {
    Exact(usize),
    Partial(usize),
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_whole_token_phrase(haystack: &str, needle: &str) -> bool {
    let h = norm_match_text(haystack);
    let n = norm_match_text(needle);
    if h.is_empty() || n.is_empty() || n.len() > h.len() {
        return false;
    }
    let hb = h.as_bytes();
    let nb = n.as_bytes();
    let first_is_word = nb.first().copied().is_some_and(is_word_byte);
    let last_is_word = nb.last().copied().is_some_and(is_word_byte);
    let mut pos = 0usize;
    while let Some(rel) = h[pos..].find(&n) {
        let start = pos + rel;
        let end = start + n.len();
        let left_ok = !first_is_word || start == 0 || !is_word_byte(hb[start - 1]);
        let right_ok = !last_is_word || end == hb.len() || !is_word_byte(hb[end]);
        if left_ok && right_ok {
            return true;
        }
        pos = start + 1;
        while pos < h.len() && !h.is_char_boundary(pos) {
            pos += 1;
        }
    }
    false
}

fn heading_path_compatible(old_hp: &[String], new_hp: &[String]) -> bool {
    if old_hp == new_hp || new_hp.is_empty() {
        return true;
    }
    !old_hp.is_empty() && old_hp.ends_with(new_hp)
}

fn build_old_to_new_block_map(
    old_indexed: &[IndexedBlock],
    new_indexed: &[IndexedBlock],
) -> HashMap<String, BlockMapKind> {
    let mut map = HashMap::new();
    if old_indexed.is_empty() || new_indexed.is_empty() {
        return map;
    }
    let old_norms: Vec<String> = old_indexed
        .iter()
        .map(|b| norm_match_text(&b.text))
        .collect();
    let new_norms: Vec<String> = new_indexed
        .iter()
        .map(|b| norm_match_text(&b.text))
        .collect();

    for (o_idx, ob) in old_indexed.iter().enumerate() {
        let norm_ob = &old_norms[o_idx];
        if norm_ob.is_empty() {
            continue;
        }
        // 1. Exact (kind, normalized_text, heading_path) match with ordinal disambiguation.
        let same_old: Vec<usize> = old_indexed
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.meta.kind == ob.meta.kind
                    && b.meta.heading_path == ob.meta.heading_path
                    && norm_match_text(&b.text) == *norm_ob
            })
            .map(|(i, _)| i)
            .collect();
        let same_new: Vec<usize> = new_indexed
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                b.meta.kind == ob.meta.kind
                    && b.meta.heading_path == ob.meta.heading_path
                    && &new_norms[*i] == norm_ob
            })
            .map(|(i, _)| i)
            .collect();

        if !same_new.is_empty() {
            if same_old.len() == same_new.len()
                && let Some(ord) = same_old.iter().position(|&i| i == o_idx)
            {
                map.insert(ob.meta.block_id.clone(), BlockMapKind::Exact(same_new[ord]));
            }
            continue;
        }

        // 2. Heading path was stripped/shortened (e.g. headings: false), require global uniqueness.
        let all_old_text_matches = old_norms.iter().filter(|t| *t == norm_ob).count();
        if all_old_text_matches != 1 {
            continue;
        }
        let compat_new: Vec<usize> = new_indexed
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                b.meta.kind == ob.meta.kind
                    && &new_norms[*i] == norm_ob
                    && heading_path_compatible(&ob.meta.heading_path, &b.meta.heading_path)
            })
            .map(|(i, _)| i)
            .collect();
        if compat_new.len() == 1 {
            map.insert(ob.meta.block_id.clone(), BlockMapKind::Exact(compat_new[0]));
            continue;
        }

        // 3. Partial block match when a block was split/trimmed, requiring >= 16 chars & >= 2 tokens & uniqueness.
        const MIN_PARTIAL_CHARS: usize = 16; // [策略] 分块裁剪后部分块匹配的最短字符数，过滤短词巧合
        let partial_new: Vec<usize> = new_indexed
            .iter()
            .enumerate()
            .filter(|(i, b)| {
                if !heading_path_compatible(&ob.meta.heading_path, &b.meta.heading_path) {
                    return false;
                }
                let nb_norm = &new_norms[*i];
                let shorter = if nb_norm.len() < norm_ob.len() {
                    nb_norm.as_str()
                } else {
                    norm_ob.as_str()
                };
                shorter.chars().count() >= MIN_PARTIAL_CHARS
                    && shorter.split_whitespace().count() >= 2
                    && (contains_whole_token_phrase(norm_ob, nb_norm)
                        || contains_whole_token_phrase(nb_norm, norm_ob))
            })
            .map(|(i, _)| i)
            .collect();
        if partial_new.len() == 1 {
            map.insert(
                ob.meta.block_id.clone(),
                BlockMapKind::Partial(partial_new[0]),
            );
        }
    }
    map
}

fn field_leaf_token(field_path: &str) -> Option<String> {
    let leaf = field_path
        .rsplit(['/', '.', '['])
        .find(|s| !s.is_empty() && !s.ends_with(']'))?;
    let mut spaced = String::with_capacity(leaf.len() + 2);
    for (i, ch) in leaf.chars().enumerate() {
        if ch == '_' || ch == '-' {
            spaced.push(' ');
        } else if ch.is_ascii_uppercase() && i > 0 {
            spaced.push(' ');
            spaced.push(ch.to_ascii_lowercase());
        } else {
            spaced.push(ch.to_ascii_lowercase());
        }
    }
    let norm = norm_match_text(&spaced);
    (!norm.is_empty()).then_some(norm)
}

fn find_metadata_island_block<'a>(
    field_path: &str,
    val_str: &str,
    indexed: &'a [IndexedBlock],
) -> Option<&'a IndexedBlock> {
    let norm_val = norm_match_text(val_str);
    if norm_val.is_empty() {
        return None;
    }
    const MIN_STANDALONE_META_CHARS: usize = 16; // [策略] 无字段名同现时元数据岛值独立匹配正文块的最短字符数
    let needs_field_context = norm_val.chars().count() < MIN_STANDALONE_META_CHARS
        || !norm_val.chars().any(|c| c.is_alphabetic());
    let leaf = field_leaf_token(field_path);
    let matches: Vec<&IndexedBlock> = indexed
        .iter()
        .filter(|b| {
            if !contains_whole_token_phrase(&b.text, &norm_val) {
                return false;
            }
            if !needs_field_context {
                return true;
            }
            let Some(ref leaf_tok) = leaf else {
                return false;
            };
            contains_whole_token_phrase(&b.text, leaf_tok)
                || b.meta
                    .heading_path
                    .iter()
                    .any(|h| contains_whole_token_phrase(h, leaf_tok))
        })
        .collect();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

fn mark_span_ungrounded(span: &mut EvidenceSpan) {
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

fn realign_span_to_filtered_blocks(
    span: &mut EvidenceSpan,
    claim_val: Option<&Value>,
    old_md: &str,
    old_indexed: &[IndexedBlock],
    old_to_new: &HashMap<String, BlockMapKind>,
    indexed: &[IndexedBlock],
) {
    let had_body_coords = span.block_id.is_some() || span.start.is_some();
    if !had_body_coords && span.method.is_metadata_island() {
        if let Some(val_str) = value_text(claim_val)
            && let Some(hit) = find_metadata_island_block(&span.field_path, &val_str, indexed)
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

    let resolved_old_bid = span.block_id.clone().or_else(|| {
        let (s, e) = (span.start?, span.end?);
        old_indexed
            .iter()
            .find(|b| b.meta.start <= s && e <= b.meta.end)
            .map(|b| b.meta.block_id.clone())
    });

    let Some(old_bid) = resolved_old_bid else {
        mark_span_ungrounded(span);
        return;
    };
    let Some(&mapped) = old_to_new.get(&old_bid) else {
        mark_span_ungrounded(span);
        return;
    };

    match mapped {
        BlockMapKind::Exact(new_idx) => {
            let hit = &indexed[new_idx];
            span.block_id = Some(hit.meta.block_id.clone());
            span.chunk_idx = hit.meta.chunk_idx;
            span.start = Some(hit.meta.start);
            span.end = Some(hit.meta.end);
            span.excerpt = hit.meta.excerpt.clone();
        }
        BlockMapKind::Partial(new_idx) => {
            let hit = &indexed[new_idx];
            let mut probe_phrases = Vec::new();
            if let Some(v) = value_text(claim_val) {
                probe_phrases.push(v);
            }
            if let (Some(s), Some(e)) = (span.start, span.end)
                && s < e
                && e <= old_md.len()
                && old_md.is_char_boundary(s)
                && old_md.is_char_boundary(e)
            {
                probe_phrases.push(old_md[s..e].to_string());
            }
            if !span.excerpt.trim().is_empty() {
                probe_phrases.push(span.excerpt.clone());
            }
            let still_present = probe_phrases
                .iter()
                .any(|p| contains_whole_token_phrase(&hit.text, p));
            if still_present {
                span.block_id = Some(hit.meta.block_id.clone());
                span.chunk_idx = hit.meta.chunk_idx;
                span.start = Some(hit.meta.start);
                span.end = Some(hit.meta.end);
                span.excerpt = hit.meta.excerpt.clone();
            } else {
                mark_span_ungrounded(span);
            }
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
            realign_span_to_filtered_blocks(ev, cval, old_md, &old_indexed, &old_to_new, &indexed);
        }
        for claim in &mut slice.claims {
            realign_span_to_filtered_blocks(
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
        version_rejected: slice.version_rejected,
        evidence: slice.evidence.clone(),
        claims: slice.claims.clone(),
    }
}
