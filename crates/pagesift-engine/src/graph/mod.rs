pub mod crawl_builder;
pub mod dom_links;
pub mod evidence;
pub mod post;

pub use post::{
    apply_http_provenance, attach_html_graph, enrich_duplicate_provenance, merge_header_alternates,
    page_delta_from_document, resync_graph_after_transform, update_graph_from_chunks,
};

use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_diff::{BlockChange, ChangeType, DiffOptions, NoiseCategory};
use pagesift_graph::{
    DocNode, DocumentInput, GraphOptions, GraphSlice, PageGraphDelta, RawFieldTrace,
    RawLinkObservation, RedirectKind, SourceKind, StreamingGraphBuilder, VersionDiffStats,
    VersionEdge, VersionRecord, build_indexed_blocks, link_version_pair,
};
use pagesift_store::{LoadedVersionSnapshot, Store, VersionCommitOutcome, VersionCommitRequest};

const MAX_VERSION_COMMIT_RETRIES: usize = 4; // [策略] 并发版本提交 CAS 重试上限
const RESOURCE_LOCK_STRIPES: usize = 64; // [策略] 同资源版本提交分片锁槽位数
const MIN_FUZZY_BLOCK_CHARS: usize = 16; // [策略] 变更块文本回退匹配最短字符阈值

fn resource_locks() -> &'static Vec<Mutex<()>> {
    static LOCKS: OnceLock<Vec<Mutex<()>>> = OnceLock::new();
    LOCKS.get_or_init(|| (0..RESOURCE_LOCK_STRIPES).map(|_| Mutex::new(())).collect())
}

fn lock_resource_stripe(
    tenant: Option<&str>,
    resource_key: &str,
) -> std::sync::MutexGuard<'static, ()> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tenant.hash(&mut h);
    resource_key.hash(&mut h);
    let idx = (h.finish() as usize) % RESOURCE_LOCK_STRIPES;
    resource_locks()[idx]
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

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
    pub tenant: Option<String>,
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
            tenant: None,
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

fn normalize_diff_block_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn match_changed_blocks(
    changes: &[BlockChange],
    doc_id: &str,
    markdown: &str,
    chunk_spans: &[(usize, usize, usize)],
    opts: &GraphOptions,
) -> Vec<String> {
    if !opts.blocks || markdown.is_empty() {
        return Vec::new();
    }
    let blocks = build_indexed_blocks(
        doc_id,
        markdown,
        chunk_spans,
        opts.limits.max_blocks_per_doc,
        opts.limits.max_excerpt_chars,
    );
    if blocks.is_empty() {
        return Vec::new();
    }
    let norm_blocks: Vec<String> = blocks
        .iter()
        .map(|b| normalize_diff_block_text(&b.text))
        .collect();
    let mut ids = Vec::new();
    let mut matched = HashSet::new();

    for change in changes {
        if !matches!(
            change.change,
            ChangeType::Added | ChangeType::Modified | ChangeType::Moved
        ) {
            continue;
        }
        let needle = change.new_text.as_deref().unwrap_or("").trim();
        let norm_needle = normalize_diff_block_text(needle);
        if norm_needle.is_empty() {
            continue;
        }

        // 1. 优先用 diff 报告的新块序号精确对齐；避免相同页脚/导航模板误命中首个副本。
        if let Some(new_idx) = change.new_index
            && new_idx < blocks.len()
            && norm_blocks[new_idx] == norm_needle
            && matched.insert(blocks[new_idx].meta.block_id.clone())
        {
            ids.push(blocks[new_idx].meta.block_id.clone());
            continue;
        }

        // 2. 精确规范化全文匹配（按与 `new_index` 最近距离消歧同文块）。
        let exact_idx = norm_blocks
            .iter()
            .enumerate()
            .filter(|(idx, bt)| {
                *bt == &norm_needle && !matched.contains(&blocks[*idx].meta.block_id)
            })
            .min_by_key(|(idx, _)| change.new_index.map_or(*idx, |ni| idx.abs_diff(ni)))
            .map(|(idx, _)| idx);
        if let Some(idx) = exact_idx
            && matched.insert(blocks[idx].meta.block_id.clone())
        {
            ids.push(blocks[idx].meta.block_id.clone());
            continue;
        }

        // 3. 段落分块边界略有差异时，仅对足够长的非琐碎文本允许唯一/最近子串回退。
        if norm_needle.chars().count() >= MIN_FUZZY_BLOCK_CHARS {
            let fuzzy_idx = norm_blocks
                .iter()
                .enumerate()
                .filter(|(idx, bt)| {
                    !matched.contains(&blocks[*idx].meta.block_id)
                        && (bt.contains(&norm_needle)
                            || (bt.chars().count() >= MIN_FUZZY_BLOCK_CHARS
                                && norm_needle.contains(bt.as_str())))
                })
                .min_by_key(|(idx, _)| change.new_index.map_or(*idx, |ni| idx.abs_diff(ni)))
                .map(|(idx, _)| idx);
            if let Some(idx) = fuzzy_idx
                && matched.insert(blocks[idx].meta.block_id.clone())
            {
                ids.push(blocks[idx].meta.block_id.clone());
            }
        }
    }
    ids
}

fn push_doc_warning(doc: &mut Document, warning: String) {
    let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
    if !ext.warnings.contains(&warning) {
        ext.warnings.push(warning);
    }
}

fn compute_version_diff(
    prev_md: Option<&str>,
    markdown: &str,
    not_modified: bool,
    doc_id: &str,
    chunk_spans: &[(usize, usize, usize)],
    opts: &GraphOptions,
) -> (Option<VersionDiffStats>, Vec<String>) {
    if not_modified && markdown.is_empty() {
        return (Some(VersionDiffStats::default()), Vec::new());
    }
    let Some(prev_md) = prev_md else {
        return (None, Vec::new());
    };
    let report = pagesift_diff::diff_with(prev_md, markdown, &version_diff_options());
    let stats = VersionDiffStats {
        old_blocks: report.stats.old_blocks,
        new_blocks: report.stats.new_blocks,
        unchanged: report.stats.unchanged,
        added: report.stats.added,
        removed: report.stats.removed,
        modified: report.stats.modified,
        moved: report.stats.moved,
        noise: report.stats.noise,
        substantive: report.stats.substantive,
    };
    let changed = match_changed_blocks(&report.blocks, doc_id, markdown, chunk_spans, opts);
    (Some(stats), changed)
}

fn load_prev_snapshot_for_version(
    gs: &pagesift_store::GraphStore<'_>,
    prev: &VersionRecord,
) -> (Option<String>, Option<String>, Option<String>) {
    match gs.load_version_snapshot(prev) {
        Ok(LoadedVersionSnapshot::Markdown(md)) => (Some(md), None, None),
        Ok(LoadedVersionSnapshot::Unavailable { reason }) => {
            let warn = format!("graph_version_diff_unavailable: {reason}");
            (None, Some(reason), Some(warn))
        }
        Err(e) => {
            let reason = format!("snapshot_read_error: {e}");
            let warn = format!("graph_version_diff_unavailable: {reason}");
            (None, Some(reason), Some(warn))
        }
    }
}

fn remap_delta_doc_id(
    delta: &mut PageGraphDelta,
    new_doc_id: String,
    observed_at_ms: u64,
    prev_version: Option<&VersionRecord>,
    diff_stats: Option<VersionDiffStats>,
    changed_block_ids: Vec<String>,
    diff_unavailable_reason: Option<String>,
) {
    let old_doc_id = std::mem::replace(&mut delta.node.doc_id, new_doc_id.clone());
    let old_prefix = format!("{old_doc_id}#");
    delta.node.observed_at_ms = observed_at_ms;

    for b in &mut delta.blocks {
        b.doc_id = new_doc_id.clone();
        b.block_id = format!("{new_doc_id}#b{}", b.block_idx);
    }
    for c in &mut delta.citations {
        if c.from_doc_id == old_doc_id {
            c.from_doc_id = new_doc_id.clone();
        }
    }
    for ev in &mut delta.evidence {
        ev.doc_id = new_doc_id.clone();
        if let Some(bid) = ev.block_id.as_mut()
            && let Some(suffix) = bid.strip_prefix(&old_prefix)
        {
            *bid = format!("{new_doc_id}#{suffix}");
        }
    }
    for claim in &mut delta.claims {
        claim.doc_id = new_doc_id.clone();
        claim.observed_at_ms = observed_at_ms;
        claim.evidence.doc_id = new_doc_id.clone();
        if let Some(bid) = claim.evidence.block_id.as_mut()
            && let Some(suffix) = bid.strip_prefix(&old_prefix)
        {
            *bid = format!("{new_doc_id}#{suffix}");
        }
    }
    for edge in &mut delta.provenance {
        match edge {
            pagesift_graph::ProvenanceEdge::AttachmentOf { parent_doc_id, .. } => {
                if parent_doc_id.as_deref() == Some(old_doc_id.as_str()) {
                    *parent_doc_id = Some(new_doc_id.clone());
                }
            }
            pagesift_graph::ProvenanceEdge::DerivedFrom {
                doc_id,
                parent_doc_id,
                ..
            } => {
                if doc_id == &old_doc_id {
                    *doc_id = new_doc_id.clone();
                }
                if parent_doc_id.as_deref() == Some(old_doc_id.as_str()) {
                    *parent_doc_id = Some(new_doc_id.clone());
                }
            }
            _ => {}
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
    let mut next_edge: Option<VersionEdge> = prev_version
        .map(|prev| link_version_pair(prev, &curr_record, diff_stats, changed_block_ids));
    if let (Some(edge), Some(reason)) = (next_edge.as_mut(), diff_unavailable_reason) {
        edge.diff_unavailable_reason = Some(reason);
    }
    delta.version = next_edge;
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
    let not_modified = doc
        .metadata
        .pagesift
        .as_ref()
        .is_some_and(|p| p.not_modified);

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
    let tenant = seed.tenant.take();
    let _stripe_guard = (opts.versions && store.is_some())
        .then(|| lock_resource_stripe(tenant.as_deref(), &resource_key));

    let mut observed_at_ms = now_ms();
    let mut previous_version: Option<VersionRecord> = None;
    let mut version_diff_stats: Option<VersionDiffStats> = None;
    let mut changed_block_ids: Vec<String> = Vec::new();
    let mut prev_md_loaded: Option<String> = None;
    let mut diff_unavailable_reason: Option<String> = None;
    let mut warnings_to_push: Vec<String> = Vec::new();

    // 仅允许同站可信 canonical 与实际重定向链参与历史别名查询和绑定，防止恶意跨站 canonical 污染他站历史。
    let trusted_canonical = DocNode::trusted_canonical_url(canonical_url.as_deref(), &final_url);
    let mut alias_candidates = BTreeSet::new();
    alias_candidates.insert(DocNode::derive_resource_key(None, &requested_url));
    alias_candidates.insert(DocNode::derive_resource_key(None, &final_url));
    for (from_u, to_u, _, _) in &seed.redirect_chain {
        alias_candidates.insert(DocNode::derive_resource_key(None, from_u));
        alias_candidates.insert(DocNode::derive_resource_key(None, to_u));
    }

    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph_for(tenant.as_deref());
        let mut lookup = vec![
            resource_key.clone(),
            final_url.clone(),
            requested_url.clone(),
        ];
        if let Some(c) = &trusted_canonical {
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
            let (prev_md, unavail_reason, unavail_warn) =
                load_prev_snapshot_for_version(&gs, &prev);
            prev_md_loaded = prev_md;
            if !not_modified {
                diff_unavailable_reason = unavail_reason;
                if let Some(w) = unavail_warn {
                    warnings_to_push.push(w);
                }
            }
            let new_doc_id = DocNode::compute_observation_doc_id(
                canonical_url.as_deref(),
                &final_url,
                observed_at_ms,
                seed.content_hash.as_deref(),
                seed.raw_hash.as_deref(),
            );
            let (stats, changed) = compute_version_diff(
                prev_md_loaded.as_deref(),
                &markdown,
                not_modified,
                &new_doc_id,
                &chunk_spans,
                opts,
            );
            version_diff_stats = stats;
            changed_block_ids = changed;
            previous_version = Some(prev);
        }
    }

    let member_deltas = seed.member_deltas;
    let seed_content_hash = seed.content_hash.clone();
    let seed_raw_hash = seed.raw_hash.clone();
    let input = DocumentInput {
        requested_url,
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
        redirect_chain: seed.redirect_chain,
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
        chunk_spans: chunk_spans.clone(),
        previous_version: previous_version.clone(),
        version_diff_stats,
        changed_block_ids,
    };

    let mut builder = StreamingGraphBuilder::new(opts.clone());
    let mut delta = builder.ingest(input);
    if let (Some(edge), Some(reason)) = (delta.version.as_mut(), diff_unavailable_reason.clone()) {
        edge.diff_unavailable_reason = Some(reason);
    }

    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph_for(tenant.as_deref());
        for attempt in 0..MAX_VERSION_COMMIT_RETRIES {
            let mut turn_aliases = alias_candidates.clone();
            if let Some(prev) = &previous_version {
                turn_aliases.insert(prev.resource_key.clone());
            }
            let alias_keys: Vec<String> = turn_aliases
                .into_iter()
                .filter(|a| a != &resource_key)
                .collect();

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

            let (snapshot_bytes, snapshot_unavail): (Option<&[u8]>, Option<&str>) =
                if !markdown.is_empty() {
                    (Some(markdown.as_bytes()), None)
                } else if not_modified && let Some(ref prev_md) = prev_md_loaded {
                    (Some(prev_md.as_bytes()), None)
                } else {
                    (None, Some("empty_markdown"))
                };

            match gs.commit_version(VersionCommitRequest {
                expected_prev_doc_id: previous_version.as_ref().map(|p| p.doc_id.as_str()),
                alias_keys: &alias_keys,
                record: &curr_record,
                edge: delta.version.as_ref(),
                snapshot_bytes,
                snapshot_unavailable_reason: snapshot_unavail,
            }) {
                Ok(VersionCommitOutcome::Committed {
                    snapshot_warning, ..
                }) => {
                    if let Some(w) = snapshot_warning {
                        warnings_to_push.push(w);
                    }
                    break;
                }
                Ok(VersionCommitOutcome::StaleParent { current_latest }) => {
                    if attempt + 1 >= MAX_VERSION_COMMIT_RETRIES {
                        warnings_to_push.push(
                            "graph_version_commit_conflict: concurrent commits exhausted retries"
                                .to_string(),
                        );
                        break;
                    }
                    // 清空本轮因旧父节点失效产生的临时提示，使用最新已提交父节点重算 diff 并重试。
                    warnings_to_push.clear();
                    previous_version = Some(current_latest);
                    diff_unavailable_reason = None;
                    observed_at_ms = now_ms();
                    if let Some(prev) = &previous_version {
                        if observed_at_ms <= prev.observed_at_ms {
                            observed_at_ms = prev.observed_at_ms + 1;
                        }
                        let (prev_md, unavail_reason, unavail_warn) =
                            load_prev_snapshot_for_version(&gs, prev);
                        prev_md_loaded = prev_md;
                        if !not_modified {
                            diff_unavailable_reason = unavail_reason;
                            if let Some(w) = unavail_warn {
                                warnings_to_push.push(w);
                            }
                        }
                    } else {
                        prev_md_loaded = None;
                    }
                    let new_doc_id = DocNode::compute_observation_doc_id(
                        canonical_url.as_deref(),
                        &final_url,
                        observed_at_ms,
                        seed_content_hash.as_deref(),
                        seed_raw_hash.as_deref(),
                    );
                    let (stats, changed) = if previous_version.is_some() {
                        compute_version_diff(
                            prev_md_loaded.as_deref(),
                            &markdown,
                            not_modified,
                            &new_doc_id,
                            &chunk_spans,
                            opts,
                        )
                    } else {
                        (None, Vec::new())
                    };
                    remap_delta_doc_id(
                        &mut delta,
                        new_doc_id,
                        observed_at_ms,
                        previous_version.as_ref(),
                        stats,
                        changed,
                        diff_unavailable_reason.clone(),
                    );
                }
                Err(e) => {
                    warnings_to_push.push(format!("graph_version_persist_failed: {e}"));
                    break;
                }
            }
        }
    }

    for w in warnings_to_push {
        push_doc_warning(doc, w);
    }

    let mut slice = GraphSlice::from(delta);
    slice.member_deltas = member_deltas;
    doc.graph = Some(slice);
}
