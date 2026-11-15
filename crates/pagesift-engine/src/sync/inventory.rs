//! 数据源资源清单（`ResourceInventory`）管理、拓扑角色分类、Scope 清洗与生命周期状态机（C18–C23 / R03）。

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
    let _ = depth;
    if path.ends_with('/')
        || [
            "/docs",
            "/blog",
            "/news",
            "/products",
            "/catalog",
            "/category",
            "/categories",
            "/collections",
            "/list",
            "/index",
            "/index.html",
            "/index.htm",
        ]
        .iter()
        .any(|p| path.ends_with(p))
    {
        return ResourceRole::Hub;
    }
    ResourceRole::Detail
}

fn origin_allowed_by_spec(spec: &DataSourceSpec, u: &Url) -> bool {
    let Some(cand_host) = u.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    let cand_port = u.port_or_known_default();
    spec.seeds.iter().any(|seed| {
        let Ok(su) = Url::parse(seed) else {
            return false;
        };
        if su.scheme() != u.scheme() || su.port_or_known_default() != cand_port {
            return false;
        }
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
    })
}

fn path_allowed_by_spec(spec: &DataSourceSpec, path: &str) -> bool {
    spec.allows_path(path)
}

/// 检查候选链接是否落在 `DataSourceSpec` 允许的站点与路径范围内，返回清洗后的规范 URL（C21）。
pub fn normalize_in_scope_url(spec: &DataSourceSpec, raw_url: &str) -> Option<String> {
    let mut u = Url::parse(raw_url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    frontier::clean(&mut u);
    if frontier::skip(&u) {
        return None;
    }
    if !origin_allowed_by_spec(spec, &u) {
        return None;
    }
    if !path_allowed_by_spec(spec, u.path()) {
        return None;
    }
    Some(crate::crawl::normalize(&u, false))
}

/// 当数据源配置更新了 `include_paths` / `exclude_paths` / `max_depth` / `seeds` 时，清理不再合规的旧库存（R03）。
pub fn prune_out_of_scope_inventory(
    spec: &DataSourceSpec,
    inventory: &mut BTreeMap<String, ResourceRecord>,
) -> usize {
    let before = inventory.len();
    inventory.retain(|_, rec| {
        rec.depth <= spec.max_depth && normalize_in_scope_url(spec, &rec.url).is_some()
    });
    before.saturating_sub(inventory.len())
}

pub(crate) fn new_discovered_record(
    norm_url: String,
    parent_url: Option<String>,
    depth: usize,
    is_seed: bool,
    importance_weight: f64,
    now_ms: u64,
) -> ResourceRecord {
    let key = DocNode::derive_resource_key(None, &norm_url);
    ResourceRecord {
        resource_key: key,
        url: norm_url.clone(),
        canonical_url: None,
        migrated_to: None,
        parent_url,
        depth,
        role: classify_resource_role(&norm_url, depth, is_seed),
        state: ResourceLifecycleState::Discovered,
        discovered_at_ms: now_ms,
        last_checked_at_ms: None,
        last_substantive_change_at_ms: None,
        next_due_at_ms: now_ms,
        etag: None,
        last_modified: None,
        last_validator_contract_id: None,
        last_authoritative_status: None,
        baseline_sufficient: false,
        has_embedded_json: false,
        text_signature: None,
        confirmed_gone_at_ms: None,
        content_hash: None,
        latest_doc_id: None,
        last_status_code: None,
        consecutive_failures: 0,
        consecutive_404s: 0,
        importance_weight,
        last_strategy: None,
        title: None,
        claim_count: 0,
    }
}

/// 确保合法且落在 Scope 内的种子 URL 已注册在资源清单中（C21）。
pub fn ensure_seeds_in_inventory(
    spec: &DataSourceSpec,
    inventory: &mut BTreeMap<String, ResourceRecord>,
    now_ms: u64,
) {
    for seed in &spec.seeds {
        let Some(norm) = normalize_in_scope_url(spec, seed) else {
            continue;
        };
        let key = DocNode::derive_resource_key(None, &norm);
        inventory
            .entry(key)
            .or_insert_with(|| new_discovered_record(norm, None, 0, true, 1.5, now_ms));
    }
}

/// 单次抓取结果对清单条目的观测输入（不再接收预计算 `next_due_at_ms`，强制先状态迁移再算退避，C19）。
#[derive(Debug, Clone)]
pub struct InventoryObservation {
    pub now_ms: u64,
    pub status_code: u16,
    pub not_modified_304: bool,
    pub is_barrier_or_error: bool,
    pub had_transport_error: bool,
    pub final_url: String,
    pub canonical_url: Option<String>,
    pub permanent_redirect_target: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub contract_id: String,
    pub capture_sufficient: bool,
    pub has_embedded_json: bool,
    pub text_signature: Option<u64>,
    pub content_hash: Option<String>,
    pub doc_id: Option<String>,
    pub title: Option<String>,
    pub claim_count: usize,
    pub substantive_changed: bool,
    pub strategy_used: String,
}

/// 将单次观测应用到资源记录，执行状态迁移与防污染更新（C18 / C20 / C21 / C23）：
/// 若发生允许范围内的永久迁移，返回需注入清单的合法目标 URL。
pub fn apply_observation_to_record(
    spec: &DataSourceSpec,
    rec: &mut ResourceRecord,
    obs: &InventoryObservation,
) -> Option<String> {
    rec.last_checked_at_ms = Some(obs.now_ms);
    if !obs.had_transport_error {
        rec.last_status_code = Some(obs.status_code);
    }
    rec.last_strategy = Some(obs.strategy_used.clone());

    if obs.not_modified_304 {
        rec.consecutive_failures = 0;
        rec.consecutive_404s = 0;
        rec.confirmed_gone_at_ms = None;
        if obs.capture_sufficient {
            rec.state = ResourceLifecycleState::Active;
            rec.baseline_sufficient = true;
            rec.last_validator_contract_id = Some(obs.contract_id.clone());
            if obs.etag.is_some() {
                rec.etag = obs.etag.clone();
            }
            if obs.last_modified.is_some() {
                rec.last_modified = obs.last_modified.clone();
            }
        } else {
            // 304 与当前契约基线不匹配：保留旧数据等待重抓，不宣称为充分快照（C15）
            rec.baseline_sufficient = false;
            rec.state = ResourceLifecycleState::TransientFailure;
        }
        return None;
    }

    if obs.status_code == 410 && !obs.had_transport_error {
        rec.state = ResourceLifecycleState::ConfirmedGone;
        rec.consecutive_404s = rec.consecutive_404s.saturating_add(1);
        rec.consecutive_failures = 0;
        rec.confirmed_gone_at_ms.get_or_insert(obs.now_ms);
        return None;
    }

    if obs.status_code == 404 && !obs.had_transport_error {
        rec.consecutive_404s = rec.consecutive_404s.saturating_add(1);
        rec.consecutive_failures = 0;
        let threshold = spec.freshness.confirm_gone_after_404s.max(1);
        if rec.consecutive_404s >= threshold {
            rec.state = ResourceLifecycleState::ConfirmedGone;
            rec.confirmed_gone_at_ms.get_or_insert(obs.now_ms);
        } else {
            rec.state = ResourceLifecycleState::SuspectedGone;
        }
        return None;
    }

    if obs.had_transport_error
        || obs.is_barrier_or_error
        || obs.status_code >= 400
        || !obs.capture_sufficient
    {
        // 非 404 故障/拦截/弱结果壳打断“连续 404”链条（C18），且绝不覆盖旧验证器与健康哈希（C02/C14）
        rec.consecutive_failures = rec.consecutive_failures.saturating_add(1);
        rec.consecutive_404s = 0;
        rec.state = ResourceLifecycleState::TransientFailure;
        return None;
    }

    if let Some(ref target) = obs.permanent_redirect_target {
        let target_key = DocNode::derive_resource_key(obs.canonical_url.as_deref(), target);
        if target_key != rec.resource_key {
            rec.state = ResourceLifecycleState::Migrated;
            rec.migrated_to = Some(target.clone());
            rec.confirmed_gone_at_ms.get_or_insert(obs.now_ms);
            rec.consecutive_failures = 0;
            rec.consecutive_404s = 0;
            return normalize_in_scope_url(spec, target);
        }
    }

    let first_baseline = rec.latest_doc_id.is_none();
    rec.state = ResourceLifecycleState::Active;
    rec.consecutive_failures = 0;
    rec.consecutive_404s = 0;
    rec.confirmed_gone_at_ms = None;
    rec.migrated_to = None;
    rec.canonical_url = obs.canonical_url.clone();

    // 权威非 304 响应：全量替换 ETag / Last-Modified（服务端不再返回时必须清除旧验证器，C23）
    rec.etag = obs.etag.clone();
    rec.last_modified = obs.last_modified.clone();
    rec.last_validator_contract_id = Some(obs.contract_id.clone());
    rec.last_authoritative_status = Some(obs.status_code);
    rec.baseline_sufficient = obs.capture_sufficient;
    rec.has_embedded_json = obs.has_embedded_json;
    if obs.text_signature.is_some() {
        rec.text_signature = obs.text_signature;
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

    let base_weight = if rec.importance_weight.is_finite() {
        rec.importance_weight
    } else {
        1.0
    };
    if first_baseline {
        rec.last_substantive_change_at_ms = Some(obs.now_ms);
    } else if obs.substantive_changed {
        rec.last_substantive_change_at_ms = Some(obs.now_ms);
        rec.importance_weight = (base_weight * 1.15).clamp(0.2, 5.0);
    } else {
        rec.importance_weight = (base_weight * 0.98).clamp(0.2, 5.0);
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
            instance_id: String::new(),
            revision: 1,
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

    fn base_obs(
        url: &str,
        now_ms: u64,
        status_code: u16,
        etag: Option<&str>,
    ) -> InventoryObservation {
        InventoryObservation {
            now_ms,
            status_code,
            not_modified_304: status_code == 304,
            is_barrier_or_error: false,
            had_transport_error: false,
            final_url: url.to_string(),
            canonical_url: None,
            permanent_redirect_target: None,
            etag: etag.map(str::to_string),
            last_modified: None,
            contract_id: "c1".to_string(),
            capture_sufficient: true,
            has_embedded_json: false,
            text_signature: Some(123),
            content_hash: Some("h1".to_string()),
            doc_id: Some("d1".to_string()),
            title: Some("Docs".to_string()),
            claim_count: 1,
            substantive_changed: false,
            strategy_used: "lightweight_http".to_string(),
        }
    }

    #[test]
    fn scope_excluded_seed_and_pruning_enforced_c21_r03() {
        let mut spec = dummy_spec();
        spec.seeds
            .push("https://example.com/docs/private/secret".to_string());
        let mut inv = BTreeMap::new();
        ensure_seeds_in_inventory(&spec, &mut inv, 1000);
        assert_eq!(inv.len(), 1);
        assert!(!inv.values().any(|r| r.url.contains("/private")));

        inv.insert(
            "https://example.com/docs/old".to_string(),
            new_discovered_record(
                "https://example.com/docs/old".to_string(),
                None,
                1,
                false,
                1.0,
                1000,
            ),
        );
        spec.exclude_paths.push("/docs/old".to_string());
        let removed = prune_out_of_scope_inventory(&spec, &mut inv);
        assert_eq!(removed, 1);
        assert_eq!(inv.len(), 1);
    }

    #[test]
    fn consecutive_404_resets_across_503_and_clears_omitted_etag_c18_c23() {
        let spec = dummy_spec();
        let mut inv = BTreeMap::new();
        ensure_seeds_in_inventory(&spec, &mut inv, 1000);
        let rec = inv.values_mut().next().unwrap();

        let url = rec.url.clone();
        apply_observation_to_record(&spec, rec, &base_obs(&url, 2000, 200, Some("\"e1\"")));
        assert_eq!(rec.etag.as_deref(), Some("\"e1\""));

        // 新 200 未携带 ETag -> 必须清除旧 ETag（C23）
        apply_observation_to_record(&spec, rec, &base_obs(&url, 2500, 200, None));
        assert_eq!(rec.etag, None);

        // 404 -> 503 -> 404 不应直接 ConfirmedGone（C18）
        apply_observation_to_record(&spec, rec, &base_obs(&url, 3000, 404, None));
        assert_eq!(rec.state, ResourceLifecycleState::SuspectedGone);
        assert_eq!(rec.consecutive_404s, 1);

        apply_observation_to_record(&spec, rec, &base_obs(&url, 4000, 503, None));
        assert_eq!(rec.state, ResourceLifecycleState::TransientFailure);
        assert_eq!(rec.consecutive_404s, 0);

        apply_observation_to_record(&spec, rec, &base_obs(&url, 5000, 404, None));
        assert_eq!(rec.state, ResourceLifecycleState::SuspectedGone);
        assert_eq!(rec.consecutive_404s, 1);

        apply_observation_to_record(&spec, rec, &base_obs(&url, 6000, 404, None));
        assert_eq!(rec.state, ResourceLifecycleState::ConfirmedGone);
        assert_eq!(rec.consecutive_404s, 2);
    }
}
