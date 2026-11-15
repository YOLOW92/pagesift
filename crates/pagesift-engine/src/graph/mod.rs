pub mod commit;
pub mod crawl_builder;
pub mod dom_links;
pub mod evidence;
pub mod post;
pub mod version_diff;

pub use post::{
    apply_http_provenance, attach_html_graph, enrich_duplicate_provenance, merge_header_alternates,
    page_delta_from_document, resync_graph_after_transform, update_graph_from_chunks,
};
pub use version_diff::version_diff_options;

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_graph::{
    DocNode, DocumentInput, GraphOptions, GraphSlice, PageGraphDelta, RawFieldTrace,
    RawLinkObservation, RedirectKind, SourceKind, StreamingGraphBuilder, TemporalSemantics,
    VersionDiffStats, VersionRecord,
};
use pagesift_store::Store;

use commit::commit_and_reconcile_version;
use version_diff::{compute_version_diff, load_prev_snapshot_for_version, resolve_304_snapshot};

const RESOURCE_LOCK_STRIPES: usize = 64; // [策略] 同资源版本提交分片锁槽位数

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
    pub if_none_match: Option<String>,
    pub if_modified_since: Option<String>,
    pub unconditional_not_modified: bool,
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
            if_none_match: None,
            if_modified_since: None,
            unconditional_not_modified: false,
        }
    }
}

/// Collect resource keys from `redirect_chain` that form a contiguous suffix of
/// same-host permanent redirects (`RedirectKind::HttpPermanent`, 301/308) leading into `final_url`.
pub fn trusted_permanent_redirect_keys(
    redirect_chain: &[(String, String, u16, RedirectKind)],
    final_url: &str,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let final_host = pagesift_graph::host_of(final_url);
    if final_host.is_empty() {
        return keys;
    }
    let mut expected_to = DocNode::derive_resource_key(None, final_url);
    for (from_u, to_u, _, kind) in redirect_chain.iter().rev() {
        if *kind != RedirectKind::HttpPermanent {
            break;
        }
        let to_key = DocNode::derive_resource_key(None, to_u);
        if to_key != expected_to {
            break;
        }
        if pagesift_graph::host_of(from_u) != final_host
            || pagesift_graph::host_of(to_u) != final_host
        {
            break;
        }
        let from_key = DocNode::derive_resource_key(None, from_u);
        keys.insert(from_key.clone());
        keys.insert(to_key);
        expected_to = from_key;
    }
    keys
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

fn push_doc_warning(doc: &mut Document, warning: String) {
    let ext = doc.metadata.pagesift.get_or_insert_with(Default::default);
    if !ext.warnings.contains(&warning) {
        ext.warnings.push(warning);
    }
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
    let prev_md_loaded: Option<String>;
    let mut matched_304_version: Option<VersionRecord> = None;
    let mut matched_304_md: Option<String> = None;
    let mut not_modified_unavail_reason: Option<String> = None;
    let mut diff_unavailable_reason: Option<String> = None;
    let mut warnings_to_push: Vec<String> = Vec::new();

    let trusted_canonical = DocNode::trusted_canonical_url(canonical_url.as_deref(), &final_url);
    let perm_redirect_keys = trusted_permanent_redirect_keys(&seed.redirect_chain, &final_url);
    let mut alias_candidates = BTreeSet::new();
    alias_candidates.insert(DocNode::derive_resource_key(None, &final_url));
    for k in &perm_redirect_keys {
        alias_candidates.insert(k.clone());
    }
    if seed.redirect_chain.is_empty()
        && pagesift_graph::host_of(&requested_url) == pagesift_graph::host_of(&final_url)
    {
        alias_candidates.insert(DocNode::derive_resource_key(None, &requested_url));
    }

    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph_for(tenant.as_deref());
        let mut raw_lookup = vec![resource_key.clone(), final_url.clone()];
        if let Some(c) = &trusted_canonical {
            raw_lookup.push(c.clone());
        }
        for k in &alias_candidates {
            raw_lookup.push(k.clone());
        }
        let mut lookup = Vec::with_capacity(raw_lookup.len());
        for cand in raw_lookup {
            if !gs
                .is_foreign_alias_pointer(&cand, &resource_key)
                .unwrap_or(false)
            {
                lookup.push(cand);
            }
        }
        let lookup_refs: Vec<&str> = lookup.iter().map(String::as_str).collect();

        if not_modified && markdown.is_empty() {
            let (m_ver, m_md, m_unavail) =
                resolve_304_snapshot(&gs, &lookup_refs, &resource_key, &final_url, &seed);
            if let Some(ref mv) = m_ver {
                if seed.content_hash.is_none() {
                    seed.content_hash = mv.content_hash.clone();
                }
                if seed.text_hash.is_none() {
                    seed.text_hash = mv.text_hash.clone();
                }
                if seed.etag.is_none() {
                    seed.etag = mv.etag.clone();
                }
                if seed.last_modified.is_none() {
                    seed.last_modified = mv.last_modified.clone();
                }
            }
            matched_304_version = m_ver;
            matched_304_md = m_md;
            not_modified_unavail_reason = m_unavail;
        }

        if let Ok(Some(prev)) = gs.latest_version_for_urls(&lookup_refs) {
            if observed_at_ms <= prev.observed_at_ms {
                observed_at_ms = prev.observed_at_ms + 1;
            }
            let (prev_md, unavail_reason, unavail_warn) =
                load_prev_snapshot_for_version(&gs, &prev);
            prev_md_loaded = prev_md;

            let new_doc_id = DocNode::compute_observation_doc_id(
                canonical_url.as_deref(),
                &final_url,
                observed_at_ms,
                seed.content_hash.as_deref(),
                seed.raw_hash.as_deref(),
            );

            if not_modified && markdown.is_empty() {
                if let Some(ref body_304) = matched_304_md {
                    let same_as_prev = matched_304_version
                        .as_ref()
                        .is_some_and(|mv| mv.doc_id == prev.doc_id);
                    if same_as_prev {
                        version_diff_stats = Some(VersionDiffStats::default());
                        changed_block_ids = Vec::new();
                    } else if prev_md_loaded.is_some() {
                        let (stats, changed) = compute_version_diff(
                            prev_md_loaded.as_deref(),
                            body_304,
                            false,
                            &new_doc_id,
                            &chunk_spans,
                            opts,
                        );
                        version_diff_stats = stats;
                        changed_block_ids = changed;
                    } else {
                        diff_unavailable_reason = unavail_reason;
                        if let Some(w) = unavail_warn {
                            warnings_to_push.push(w);
                        }
                    }
                } else {
                    let reason = not_modified_unavail_reason
                        .clone()
                        .unwrap_or_else(|| "validator_mismatch".to_string());
                    warnings_to_push.push(format!("graph_version_diff_unavailable: {reason}"));
                    diff_unavailable_reason = Some(reason);
                }
            } else {
                diff_unavailable_reason = unavail_reason;
                if let Some(w) = unavail_warn {
                    warnings_to_push.push(w);
                }
                let (stats, changed) = compute_version_diff(
                    prev_md_loaded.as_deref(),
                    &markdown,
                    false,
                    &new_doc_id,
                    &chunk_spans,
                    opts,
                );
                version_diff_stats = stats;
                changed_block_ids = changed;
            }
            previous_version = Some(prev);
        } else if not_modified && markdown.is_empty() && matched_304_md.is_none() {
            let reason = not_modified_unavail_reason
                .clone()
                .unwrap_or_else(|| "validator_mismatch".to_string());
            warnings_to_push.push(format!("graph_version_diff_unavailable: {reason}"));
        }
    }

    let member_deltas = seed.member_deltas;
    let seed_content_hash = seed.content_hash.clone();
    let seed_raw_hash = seed.raw_hash.clone();
    let seed_last_modified = seed.last_modified.clone();
    let seed_json_ld = seed.json_ld.clone();
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

    let temporal = TemporalSemantics::from_document_sources(
        delta.node.observed_at_ms,
        doc.metadata.published_time.as_deref(),
        doc.metadata.modified_time.as_deref(),
        seed_last_modified.as_deref(),
        &seed_json_ld,
    );
    if temporal.published_at.is_some()
        || temporal.modified_at.is_some()
        || temporal.valid_from.is_some()
        || temporal.valid_until.is_some()
    {
        delta.temporal = Some(temporal.clone());
        for claim in &mut delta.claims {
            claim.temporal = Some(temporal.clone());
        }
    }

    if opts.versions
        && let Some(st) = store
    {
        let gs = st.graph_for(tenant.as_deref());
        commit_and_reconcile_version(
            &gs,
            opts,
            &mut delta,
            &resource_key,
            &alias_candidates,
            canonical_url.as_deref(),
            &final_url,
            &markdown,
            not_modified,
            matched_304_version.as_ref(),
            matched_304_md.as_deref(),
            not_modified_unavail_reason.as_deref(),
            seed_content_hash.as_deref(),
            seed_raw_hash.as_deref(),
            &chunk_spans,
            previous_version,
            &mut warnings_to_push,
        );
    }

    for w in warnings_to_push {
        push_doc_warning(doc, w);
    }

    let mut slice = GraphSlice::from(delta);
    slice.member_deltas = member_deltas;
    doc.graph = Some(slice);
}
