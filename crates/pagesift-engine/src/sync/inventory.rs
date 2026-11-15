//! 数据源资源清单（`ResourceInventory`）管理、拓扑角色分类与防误删状态机迁移。

use std::collections::BTreeMap;

use pagesift_graph::DocNode;
use pagesift_net::Url;
use pagesift_store::{DataSourceSpec, ResourceLifecycleState, ResourceRecord, ResourceRole};

use crate::frontier;

/// 根据 URL 形态与深度推断资源在数据源中的拓扑角色。
pub fn classify_resource_role(url: &str, depth: usize, is_seed: bool) -> ResourceRole {
    if is_seed {
        return ResourceRole::Seed;
    }
    let lower = url.to_ascii_lowercase();
    let path = Url::parse(&lower)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| lower.clone());

    if path.ends_with(".xml")
        || path.ends_with("/feed")
        || path.ends_with("/rss")
        || path.ends_with("/atom")
        || path.contains("sitemap")
    {
        return ResourceRole::FeedOrSitemap;
    }
    if [".pdf", ".docx", ".xlsx", ".pptx", ".csv", ".zip"]
        .iter()
        .any(|ext| path.ends_with(ext))
    {
        return ResourceRole::Attachment;
    }
    if lower.contains("page=")
        || lower.contains("offset=")
        || lower.contains("cursor=")
        || path.contains("/page/")
    {
        return ResourceRole::Pagination;
    }
    if depth <= 1
        || path.ends_with('/')
        || [
            "/docs",
            "/blog",
            "/news",
            "/products",
            "/catalog",
            "/list",
            "/index",
        ]
        .iter()
        .any(|p| path.ends_with(p))
    {
        return ResourceRole::Hub;
    }
    ResourceRole::Detail
}

/// 检查候选链接是否落在 `DataSourceSpec` 允许的站点与路径范围内，返回清洗后的规范 URL。
pub fn normalize_in_scope_url(spec: &DataSourceSpec, raw_url: &str) -> Option<String> {
    let mut u = Url::parse(raw_url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    frontier::clean(&mut u);
    if frontier::skip(&u) {
        return None;
    }
    let cand_host = u.host_str()?.to_ascii_lowercase();

    let host_allowed = spec.seeds.iter().any(|seed| {
        let Ok(su) = Url::parse(seed) else {
            return false;
        };
        let Some(sh) = su.host_str().map(str::to_ascii_lowercase) else {
            return false;
        };
        if cand_host == sh
            || cand_host.strip_prefix("www.") == Some(sh.as_str())
            || sh.strip_prefix("www.") == Some(cand_host.as_str())
        {
            return true;
        }
        spec.allow_subdomains && cand_host.ends_with(&format!(".{sh}"))
    });
    if !host_allowed {
        return None;
    }

    let path = u.path();
    if !spec.include_paths.is_empty()
        && !spec
            .include_paths
            .iter()
            .any(|inc| path.starts_with(inc.as_str()))
    {
        return None;
    }
    if spec
        .exclude_paths
        .iter()
        .any(|exc| !exc.is_empty() && path.starts_with(exc.as_str()))
    {
        return None;
    }

    Some(crate::crawl::normalize(&u, false))
}

/// 确保种子 URL 均已注册在资源清单中。
pub fn ensure_seeds_in_inventory(
    spec: &DataSourceSpec,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    now_ms: u64,
) {
    for seed in &spec.seeds {
        let norm = normalize_in_scope_url(spec, seed).unwrap_or_else(|| seed.clone());
        let key = DocNode::derive_resource_key(None, &norm);
        inventory
            .entry(key.clone())
            .or_insert_with(|| ResourceRecord {
                resource_key: key,
                url: norm,
                canonical_url: None,
                migrated_to: None,
                parent_url: None,
                depth: 0,
                role: ResourceRole::Seed,
                state: ResourceLifecycleState::Discovered,
                discovered_at_ms: now_ms,
                last_checked_at_ms: None,
                last_substantive_change_at_ms: None,
                next_due_at_ms: now_ms,
                etag: None,
                last_modified: None,
                content_hash: None,
                latest_doc_id: None,
                last_status_code: None,
                consecutive_failures: 0,
                consecutive_404s: 0,
                importance_weight: 1.5,
                last_strategy: None,
                title: None,
                claim_count: 0,
            });
    }
}

/// 单次抓取结果对清单条目的观测输入。
#[derive(Debug, Clone)]
pub struct InventoryObservation {
    pub now_ms: u64,
    pub status_code: u16,
    pub not_modified_304: bool,
    pub is_barrier_or_error: bool,
    pub final_url: String,
    pub canonical_url: Option<String>,
    pub permanent_redirect_target: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: Option<String>,
    pub doc_id: Option<String>,
    pub title: Option<String>,
    pub claim_count: usize,
    pub substantive_changed: bool,
    pub strategy_used: String,
    pub next_due_at_ms: u64,
}

/// 将单次观测应用到资源记录，执行防误删生命周期迁移：
/// - 单次 404 先进入 `SuspectedGone`，达到 `confirm_gone_after_404s` 或遇 410 才进入 `ConfirmedGone`；
/// - 5xx / 429 / 网络错误 / 拦截挑战进入 `TransientFailure`，保留历史快照哈希与校验子；
/// - 永久重定向或规范 URL 迁移进入 `Migrated`，并返回需要注入清单的新目标 URL。
pub fn apply_observation_to_record(
    spec: &DataSourceSpec,
    rec: &mut ResourceRecord,
    obs: &InventoryObservation,
) -> Option<String> {
    rec.last_checked_at_ms = Some(obs.now_ms);
    rec.last_status_code = Some(obs.status_code);
    rec.last_strategy = Some(obs.strategy_used.clone());
    rec.next_due_at_ms = obs.next_due_at_ms;

    if obs.not_modified_304 {
        rec.state = ResourceLifecycleState::Active;
        rec.consecutive_failures = 0;
        rec.consecutive_404s = 0;
        if obs.etag.is_some() {
            rec.etag = obs.etag.clone();
        }
        if obs.last_modified.is_some() {
            rec.last_modified = obs.last_modified.clone();
        }
        return None;
    }

    if obs.status_code == 410 {
        rec.state = ResourceLifecycleState::ConfirmedGone;
        rec.consecutive_404s = rec.consecutive_404s.saturating_add(1);
        return None;
    }

    if obs.status_code == 404 {
        rec.consecutive_404s = rec.consecutive_404s.saturating_add(1);
        let threshold = spec.freshness.confirm_gone_after_404s.max(1);
        if rec.consecutive_404s >= threshold {
            rec.state = ResourceLifecycleState::ConfirmedGone;
        } else {
            rec.state = ResourceLifecycleState::SuspectedGone;
        }
        return None;
    }

    if obs.is_barrier_or_error || obs.status_code >= 400 {
        rec.consecutive_failures = rec.consecutive_failures.saturating_add(1);
        rec.state = ResourceLifecycleState::TransientFailure;
        return None;
    }

    // 检查是否发生同站永久重定向或规范 URL 迁移
    if let Some(ref target) = obs.permanent_redirect_target {
        let target_key = DocNode::derive_resource_key(obs.canonical_url.as_deref(), target);
        if target_key != rec.resource_key {
            rec.state = ResourceLifecycleState::Migrated;
            rec.migrated_to = Some(target.clone());
            rec.consecutive_failures = 0;
            rec.consecutive_404s = 0;
            return Some(target.clone());
        }
    }

    rec.state = ResourceLifecycleState::Active;
    rec.consecutive_failures = 0;
    rec.consecutive_404s = 0;
    rec.canonical_url = obs.canonical_url.clone();
    if obs.etag.is_some() {
        rec.etag = obs.etag.clone();
    }
    if obs.last_modified.is_some() {
        rec.last_modified = obs.last_modified.clone();
    }
    if obs.content_hash.is_some() {
        rec.content_hash = obs.content_hash.clone();
    }
    if obs.doc_id.is_some() {
        rec.latest_doc_id = obs.doc_id.clone();
    }
    if obs.title.is_some() {
        rec.title = obs.title.clone();
    }
    rec.claim_count = obs.claim_count;

    if obs.substantive_changed || rec.last_substantive_change_at_ms.is_none() {
        rec.last_substantive_change_at_ms = Some(obs.now_ms);
        rec.importance_weight = (rec.importance_weight * 1.15).clamp(0.2, 5.0);
    } else {
        rec.importance_weight = (rec.importance_weight * 0.98).clamp(0.2, 5.0);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_store::{FreshnessPolicy, SyncBudget};

    fn dummy_spec() -> DataSourceSpec {
        DataSourceSpec {
            id: "s1".to_string(),
            name: None,
            seeds: vec!["https://example.com/docs/".to_string()],
            include_paths: vec!["/docs".to_string()],
            exclude_paths: vec!["/docs/private".to_string()],
            max_depth: 2,
            allow_subdomains: false,
            freshness: FreshnessPolicy::default(),
            budget: SyncBudget::default(),
            extract_schema: None,
            extract_prompt: None,
            required_fields: Vec::new(),
            allow_browser: true,
            created_at_ms: 1000,
            updated_at_ms: 1000,
        }
    }

    #[test]
    fn scope_and_lifecycle_transitions_prevent_false_disappearance() {
        let spec = dummy_spec();
        assert!(normalize_in_scope_url(&spec, "https://example.com/docs/guide").is_some());
        assert!(normalize_in_scope_url(&spec, "https://example.com/docs/private/a").is_none());
        assert!(normalize_in_scope_url(&spec, "https://other.com/docs/guide").is_none());

        let mut inv = BTreeMap::new();
        ensure_seeds_in_inventory(&spec, &mut inv, 1000);
        let rec = inv.values_mut().next().unwrap();

        // 首次 200 激活
        apply_observation_to_record(
            &spec,
            rec,
            &InventoryObservation {
                now_ms: 2000,
                status_code: 200,
                not_modified_304: false,
                is_barrier_or_error: false,
                final_url: rec.url.clone(),
                canonical_url: None,
                permanent_redirect_target: None,
                etag: Some("\"e1\"".to_string()),
                last_modified: None,
                content_hash: Some("h1".to_string()),
                doc_id: Some("d1".to_string()),
                title: Some("Docs".to_string()),
                claim_count: 2,
                substantive_changed: true,
                strategy_used: "lightweight_http".to_string(),
                next_due_at_ms: 3000,
            },
        );
        assert_eq!(rec.state, ResourceLifecycleState::Active);

        // 临时 503 故障保留历史哈希与 ETag
        apply_observation_to_record(
            &spec,
            rec,
            &InventoryObservation {
                now_ms: 3000,
                status_code: 503,
                not_modified_304: false,
                is_barrier_or_error: false,
                final_url: rec.url.clone(),
                canonical_url: None,
                permanent_redirect_target: None,
                etag: None,
                last_modified: None,
                content_hash: None,
                doc_id: None,
                title: None,
                claim_count: 0,
                substantive_changed: false,
                strategy_used: "conditional_http".to_string(),
                next_due_at_ms: 3500,
            },
        );
        assert_eq!(rec.state, ResourceLifecycleState::TransientFailure);
        assert_eq!(rec.etag.as_deref(), Some("\"e1\""));

        // 单次 404 先进入 SuspectedGone，第二次 404 才进入 ConfirmedGone
        let mut obs_404 = InventoryObservation {
            now_ms: 4000,
            status_code: 404,
            not_modified_304: false,
            is_barrier_or_error: false,
            final_url: rec.url.clone(),
            canonical_url: None,
            permanent_redirect_target: None,
            etag: None,
            last_modified: None,
            content_hash: None,
            doc_id: None,
            title: None,
            claim_count: 0,
            substantive_changed: false,
            strategy_used: "conditional_http".to_string(),
            next_due_at_ms: 4500,
        };
        apply_observation_to_record(&spec, rec, &obs_404);
        assert_eq!(rec.state, ResourceLifecycleState::SuspectedGone);
        obs_404.now_ms = 5000;
        apply_observation_to_record(&spec, rec, &obs_404);
        assert_eq!(rec.state, ResourceLifecycleState::ConfirmedGone);
    }
}
