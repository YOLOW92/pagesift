pub mod crawl_builder;
pub mod dom_links;
pub mod evidence;
pub mod post;

pub use post::{
    apply_http_provenance, attach_html_graph, enrich_duplicate_provenance, merge_header_alternates,
    page_delta_from_document, update_graph_from_chunks,
};

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_diff::{BlockChange, ChangeType, DiffOptions, NoiseCategory};
use pagesift_graph::{
    DocNode, DocumentInput, GraphOptions, GraphSlice, PageGraphDelta, RawFieldTrace,
    RawLinkObservation, RedirectKind, SourceKind, StreamingGraphBuilder, VersionDiffStats,
    VersionRecord, build_indexed_blocks,
};
use pagesift_store::Store;

#[derive(Debug, Clone)]
pub struct PageGraphSeed {
    pub source_kind: SourceKind,
    pub engine: String,
    pub canonical_from_header: bool,
    pub content_hash: Option<String>,
    pub raw_hash: Option<String>,
    pub text_hash: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub redirect_chain: Vec<(String, String, u16, RedirectKind)>,
    pub links: Vec<RawLinkObservation>,
    pub alternates: Vec<(String, Option<String>, Option<String>)>,
    pub attachments: Vec<(String, Option<String>, Option<String>)>,
    pub json_ld: Vec<serde_json::Value>,
    pub raw_traces: Vec<RawFieldTrace>,
    pub member_deltas: Vec<PageGraphDelta>,
}

impl Default for PageGraphSeed {
    fn default() -> Self {
        Self {
            source_kind: SourceKind::Html,
            engine: "http".to_string(),
            canonical_from_header: false,
            content_hash: None,
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            redirect_chain: Vec::new(),
            links: Vec::new(),
            alternates: Vec::new(),
            attachments: Vec::new(),
            json_ld: Vec::new(),
            raw_traces: Vec::new(),
            member_deltas: Vec::new(),
        }
    }
}

/// Resolve effective DataGraph options when enabled via `opts.graph` or `formats: ["graph"]`.
pub fn effective_graph_options(opts: &ScrapeOptions) -> Option<GraphOptions> {
    if let Some(g) = &opts.graph {
        if g.enabled {
            return Some(g.clone());
        }
        if opts.wants("graph") {
            let mut copy = g.clone();
            copy.enabled = true;
            return Some(copy);
        }
        return None;
    }
    if opts.wants("graph") {
        return Some(GraphOptions::enabled());
    }
    None
}

pub fn redirect_kind(status: u16) -> RedirectKind {
    RedirectKind::from_status(status)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Keep dates/relative-times/counts as noise while treating prices & numbers as substantive changes.
pub fn version_diff_options() -> DiffOptions {
    DiffOptions {
        noise_categories: vec![
            NoiseCategory::Date,
            NoiseCategory::RelativeTime,
            NoiseCategory::Count,
        ],
        ..Default::default()
    }
}

fn snapshot_key(resource_key: &str) -> String {
    format!("graph\0{resource_key}")
}

fn load_previous_markdown(store: &Store, resource_keys: &[String]) -> Option<String> {
    let snaps = store.snapshots();
    for rk in resource_keys {
        let key = snapshot_key(rk);
        if let Ok(Some(meta)) = snaps.latest(&key)
            && let Ok(Some(bytes)) = snaps.get(&meta.hash)
            && let Ok(text) = String::from_utf8(bytes)
        {
            return Some(text);
        }
    }
    None
}

fn match_changed_blocks(
    changes: &[BlockChange],
    doc_id: &str,
    markdown: &str,
    chunk_spans: &[(usize, usize, usize)],
    opts: &GraphOptions,
) -> Vec<String> {
    let blocks = build_indexed_blocks(
        doc_id,
        markdown,
        chunk_spans,
        opts.limits.max_blocks_per_doc,
        opts.limits.max_excerpt_chars,
    );
    let mut ids = Vec::new();
    for change in changes {
        if !matches!(
            change.change,
            ChangeType::Added | ChangeType::Modified | ChangeType::Moved
        ) {
            continue;
        }
        let needle = change.new_text.as_deref().unwrap_or("").trim();
        if needle.is_empty() {
            continue;
        }
        let prefix: String = needle.chars().take(48).collect(); // [策略] 变更块定位前缀字符数
        for b in &blocks {
            if (b.text.contains(&prefix) || needle.contains(b.text.trim()))
                && !ids.contains(&b.meta.block_id)
            {
                ids.push(b.meta.block_id.clone());
            }
        }
    }
    ids
}

/// Build per-document DataGraph slice, run version diffing & store persistence when enabled.
pub fn build_and_attach_page_graph(
    store: Option<&Store>,
    opts: &GraphOptions,
    doc: &mut Document,
    mut seed: PageGraphSeed,
) {
    let final_url = doc
        .metadata
        .url
        .clone()
        .or_else(|| doc.metadata.source_url.clone())
        .unwrap_or_default();
    if final_url.is_empty() {
        return;
    }
    let requested_url = doc
        .metadata
        .source_url
        .clone()
        .unwrap_or_else(|| final_url.clone());
    let canonical_url = doc.metadata.canonical_url.clone();
    let markdown = doc.markdown.clone().unwrap_or_default();
    let chunk_spans = evidence::chunk_spans_of(doc.chunks.as_deref());

    if seed.alternates.is_empty() && !doc.alternates.is_empty() {
        seed.alternates = doc
            .alternates
            .iter()
            .map(|(lang, url)| {
                (
                    url.clone(),
                    None,
                    (!lang.trim().is_empty()).then(|| lang.clone()),
                )
            })
            .collect();
    }
    if seed.raw_traces.is_empty()
        && !markdown.is_empty()
        && seed.source_kind != SourceKind::Html
        && (opts.evidence || opts.facts || opts.conflicts)
    {
        seed.raw_traces = evidence::collect_tabular_and_kv_traces(&markdown, Some(&final_url));
    }

    let resource_key = DocNode::derive_resource_key(canonical_url.as_deref(), &final_url);
    let mut observed_at_ms = now_ms();

    let mut previous_version: Option<VersionRecord> = None;
    let mut version_diff_stats: Option<VersionDiffStats> = None;
    let mut changed_block_ids: Vec<String> = Vec::new();

    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph();
        let mut lookup = vec![
            resource_key.clone(),
            final_url.clone(),
            requested_url.clone(),
        ];
        if let Some(c) = &canonical_url {
            lookup.push(c.clone());
        }
        for (from_u, to_u, _, _) in &seed.redirect_chain {
            lookup.push(from_u.clone());
            lookup.push(to_u.clone());
        }
        let lookup_refs: Vec<&str> = lookup.iter().map(String::as_str).collect();
        if let Ok(Some(prev)) = gs.latest_version_for_urls(&lookup_refs) {
            if observed_at_ms <= prev.observed_at_ms {
                observed_at_ms = prev.observed_at_ms + 1;
            }
            let snap_keys = vec![prev.resource_key.clone(), resource_key.clone()];
            if let Some(prev_md) = load_previous_markdown(st, &snap_keys) {
                let report = pagesift_diff::diff_with(&prev_md, &markdown, &version_diff_options());
                version_diff_stats = Some(VersionDiffStats {
                    old_blocks: report.stats.old_blocks,
                    new_blocks: report.stats.new_blocks,
                    unchanged: report.stats.unchanged,
                    added: report.stats.added,
                    removed: report.stats.removed,
                    modified: report.stats.modified,
                    moved: report.stats.moved,
                    noise: report.stats.noise,
                    substantive: report.stats.substantive,
                });
                let new_doc_id = DocNode::compute_doc_id(
                    &final_url,
                    observed_at_ms,
                    seed.content_hash.as_deref(),
                );
                changed_block_ids = match_changed_blocks(
                    &report.blocks,
                    &new_doc_id,
                    &markdown,
                    &chunk_spans,
                    opts,
                );
            }
            previous_version = Some(prev);
        }
    }

    let member_deltas = seed.member_deltas;
    let input = DocumentInput {
        requested_url: requested_url.clone(),
        final_url: final_url.clone(),
        canonical_url: canonical_url.clone(),
        canonical_from_header: seed.canonical_from_header,
        source_kind: seed.source_kind,
        observed_at_ms,
        status_code: doc.metadata.status_code,
        engine: seed.engine,
        content_hash: seed.content_hash,
        raw_hash: seed.raw_hash,
        text_hash: seed.text_hash,
        etag: seed.etag,
        last_modified: seed.last_modified,
        title: doc.metadata.title.clone(),
        markdown: markdown.clone(),
        redirect_chain: seed.redirect_chain.clone(),
        links: seed.links,
        alternates: seed.alternates,
        duplicate_of: doc
            .metadata
            .pagesift
            .as_ref()
            .and_then(|p| p.duplicate_of.clone()),
        attachments: seed.attachments,
        structured_json_ld: seed.json_ld,
        extracted_fields: doc.extract.clone(),
        raw_traces: seed.raw_traces,
        chunk_spans,
        previous_version: previous_version.clone(),
        version_diff_stats,
        changed_block_ids,
    };

    let mut builder = StreamingGraphBuilder::new(opts.clone());
    let delta = builder.ingest(input);

    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph();
        let mut alias_candidates = BTreeSet::new();
        alias_candidates.insert(DocNode::derive_resource_key(None, &requested_url));
        alias_candidates.insert(DocNode::derive_resource_key(None, &final_url));
        for (from_u, to_u, _, _) in &seed.redirect_chain {
            alias_candidates.insert(DocNode::derive_resource_key(None, from_u));
            alias_candidates.insert(DocNode::derive_resource_key(None, to_u));
        }
        if let Some(prev) = &previous_version {
            alias_candidates.insert(prev.resource_key.clone());
        }
        for alias in alias_candidates {
            if alias != resource_key {
                let _ = gs.bind_alias(&alias, &resource_key);
            }
        }
        let curr_record = VersionRecord {
            resource_key: delta.node.resource_key.clone(),
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
        let _ = gs.record_version(&curr_record, delta.version.as_ref());
        if !markdown.is_empty() {
            let _ = st
                .snapshots()
                .put(&snapshot_key(&resource_key), markdown.as_bytes());
        }
    }

    let mut slice = GraphSlice::from(delta);
    slice.member_deltas = member_deltas;
    doc.graph = Some(slice);
}
