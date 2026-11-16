//! 跨周期路径族经验表（`FamilyExperienceTable`）与可证伪 URL 改写规则（`LearnedUrlRule`）。
//!
//! 1.5 Loop 把单次请求内的统计学习扩展为跨同步周期持久化的经验层：
//! - 按路径族（`origin + 首级路径`）记录 HTTP 充足率、浏览器增益率、304 条件命中率、
//!   实质内容变化率（泊松过程 `ChangeRate`，排除纯噪声变化）、声明产出率、拦截率与平均耗时；
//! - 每次观测按遗忘因子 `1 - δ` 衰减历史 Beta 计数，保证站点改版后自动重学；
//! - 记录从重复页面对学到的 URL 改写规则，支持跨周期复用与“证伪即撤销”（`Revoked`）。

use std::collections::HashMap;
use std::sync::Mutex;

use pagesift_stat::Delta;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::render::family_key;

#[path = "experience_rates.rs"]
mod experience_rates;
#[path = "experience_transfer.rs"]
pub mod experience_transfer;
pub use experience_rates::{
    BetaSnapshot, ChangeRateSnapshot, FamilyObservation, RegularitySnapshot,
};
pub use experience_transfer::TransferMeta;

const DEFAULT_MIN_RULE_CONFIRMATIONS: u32 = 2; // [推导] 对应 p₀ = √δ 下 p₀^k ≤ δ 的最小独立证实数 k* = 2
const SURPRISE_BETA_KEEP: f64 = 0.35;

/// 单个路径族的跨周期经验统计。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyStats {
    pub family: String,
    pub http_sufficient: BetaSnapshot,
    pub browser_gain: BetaSnapshot,
    pub conditional_hit: BetaSnapshot,
    #[serde(default)]
    pub embedded_json: BetaSnapshot,
    pub substantive_change: ChangeRateSnapshot,
    pub claim_yield: BetaSnapshot,
    pub barrier_rate: BetaSnapshot,
    pub ewma_cost_ms: f64,
    #[serde(default)]
    pub ewma_304_cost_ms: f64,
    #[serde(default)]
    pub ewma_http_cost_ms: f64,
    #[serde(default)]
    pub ewma_browser_cost_ms: f64,
    #[serde(default)]
    pub browser_failure_rate: BetaSnapshot,
    #[serde(default)]
    pub consecutive_direct_browser: u32,
    #[serde(default)]
    pub drift_count: u32,
    #[serde(default)]
    pub transfer: TransferMeta,
    pub total_observations: u64,
    pub last_observed_at_ms: u64,
}

impl FamilyStats {
    pub fn new(family: impl Into<String>) -> Self {
        Self {
            family: family.into(),
            http_sufficient: BetaSnapshot::default(),
            browser_gain: BetaSnapshot::default(),
            conditional_hit: BetaSnapshot::default(),
            embedded_json: BetaSnapshot::default(),
            substantive_change: ChangeRateSnapshot::default(),
            claim_yield: BetaSnapshot::default(),
            barrier_rate: BetaSnapshot::default(),
            ewma_cost_ms: 0.0,
            ewma_304_cost_ms: 0.0,
            ewma_http_cost_ms: 0.0,
            ewma_browser_cost_ms: 0.0,
            browser_failure_rate: BetaSnapshot::default(),
            consecutive_direct_browser: 0,
            drift_count: 0,
            transfer: TransferMeta::default(),
            total_observations: 0,
            last_observed_at_ms: 0,
        }
    }

    pub fn observe(&mut self, obs: &FamilyObservation) {
        if !obs.has_signal() {
            return;
        }
        let keep = 1.0 - Delta::DEFAULT.get();

        // 若该家族正处于跨族迁移验证窗，先校验本地独立事实，必要时触发负迁移抹除
        experience_transfer::validate_or_reject_transfer(self, obs);

        // 不可靠观测（uncertain_decode / truncated / invalid 304）仅记录外显成本/屏障，不污染核心充分性与变化率
        if !obs.unreliable_observation {
            if let Some(http_ok) = obs.http_sufficient {
                if self
                    .http_sufficient
                    .observe_with_surprise(http_ok, keep, SURPRISE_BETA_KEEP)
                {
                    self.drift_count = self.drift_count.saturating_add(1);
                }
                self.consecutive_direct_browser = 0;
            } else if obs.used_browser {
                self.consecutive_direct_browser = self.consecutive_direct_browser.saturating_add(1);
            }
            if let Some(gain) = obs.browser_gain
                && self
                    .browser_gain
                    .observe_with_surprise(gain, keep, SURPRISE_BETA_KEEP)
            {
                self.drift_count = self.drift_count.saturating_add(1);
            }
            if let Some(hit) = obs.conditional_hit {
                self.conditional_hit.observe_decayed(hit, keep);
            }
            if let Some(ej) = obs.embedded_json_observed
                && self
                    .embedded_json
                    .observe_with_surprise(ej, keep, SURPRISE_BETA_KEEP)
            {
                self.drift_count = self.drift_count.saturating_add(1);
            }
            if let Some((dt, changed)) = obs.substantive_change
                && self.substantive_change.observe_at(
                    dt,
                    changed,
                    obs.observed_at_ms,
                    obs.last_modified_at_ms,
                )
            {
                self.drift_count = self.drift_count.saturating_add(1);
            }
            if let Some(yielded) = obs.yielded_claims_or_links {
                self.claim_yield.observe_decayed(yielded, keep);
            }
        }
        if let Some(barrier) = obs.encountered_barrier {
            self.barrier_rate.observe_decayed(barrier, keep);
        }
        if let Some(b_fail) = obs.browser_failed {
            self.browser_failure_rate.observe_decayed(b_fail, keep);
        }

        let alpha = Delta::DEFAULT.get();
        let update_ewma = |cur: &mut f64, sample: f64| {
            if sample >= 0.0 && sample.is_finite() {
                if *cur <= 0.0 {
                    *cur = sample;
                } else {
                    *cur = (1.0 - alpha) * *cur + alpha * sample;
                }
            }
        };
        if obs.cost_ms >= 0.0 && obs.cost_ms.is_finite() {
            if self.total_observations == 0 {
                self.ewma_cost_ms = obs.cost_ms;
            } else {
                self.ewma_cost_ms = (1.0 - alpha) * self.ewma_cost_ms + alpha * obs.cost_ms;
            }
        }
        if obs.is_304 {
            update_ewma(
                &mut self.ewma_304_cost_ms,
                obs.http_cost_ms.unwrap_or(obs.cost_ms),
            );
        } else if !obs.used_browser {
            update_ewma(
                &mut self.ewma_http_cost_ms,
                obs.http_cost_ms.unwrap_or(obs.cost_ms),
            );
        } else {
            if let Some(hc) = obs.http_cost_ms {
                update_ewma(&mut self.ewma_http_cost_ms, hc);
            }
            update_ewma(
                &mut self.ewma_browser_cost_ms,
                obs.browser_cost_ms.unwrap_or(obs.cost_ms),
            );
        }

        self.total_observations = self.total_observations.saturating_add(1);
        if obs.observed_at_ms > self.last_observed_at_ms {
            self.last_observed_at_ms = obs.observed_at_ms;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.http_sufficient.merge(&other.http_sufficient);
        self.browser_gain.merge(&other.browser_gain);
        self.conditional_hit.merge(&other.conditional_hit);
        self.embedded_json.merge(&other.embedded_json);
        self.substantive_change.merge(&other.substantive_change);
        self.claim_yield.merge(&other.claim_yield);
        self.barrier_rate.merge(&other.barrier_rate);
        self.browser_failure_rate.merge(&other.browser_failure_rate);
        let avg_nonzero = |a: f64, b: f64| -> f64 {
            if a <= 0.0 {
                b
            } else if b <= 0.0 {
                a
            } else {
                0.5 * (a + b)
            }
        };
        self.ewma_cost_ms = avg_nonzero(self.ewma_cost_ms, other.ewma_cost_ms);
        self.ewma_304_cost_ms = avg_nonzero(self.ewma_304_cost_ms, other.ewma_304_cost_ms);
        self.ewma_http_cost_ms = avg_nonzero(self.ewma_http_cost_ms, other.ewma_http_cost_ms);
        self.ewma_browser_cost_ms =
            avg_nonzero(self.ewma_browser_cost_ms, other.ewma_browser_cost_ms);
        self.consecutive_direct_browser = self
            .consecutive_direct_browser
            .min(other.consecutive_direct_browser);
        self.drift_count = self.drift_count.max(other.drift_count);
        if self.transfer.donor_description.is_none() && other.transfer.donor_description.is_some() {
            self.transfer = other.transfer.clone();
        }
        self.total_observations = self
            .total_observations
            .saturating_add(other.total_observations);
        self.last_observed_at_ms = self.last_observed_at_ms.max(other.last_observed_at_ms);
    }

    /// 返回按采集模态区分的期望成本估计（ms）。
    pub fn expected_cost_for(&self, strategy: crate::strategy::AcquisitionStrategy) -> f64 {
        use crate::strategy::AcquisitionStrategy;
        let c_http = if self.ewma_http_cost_ms > 0.0 {
            self.ewma_http_cost_ms
        } else if self.ewma_cost_ms > 0.0 {
            self.ewma_cost_ms
        } else {
            55.0
        };
        let c_304 = if self.ewma_304_cost_ms > 0.0 {
            self.ewma_304_cost_ms
        } else {
            (c_http * 0.35).max(12.0)
        };
        let c_browser = if self.ewma_browser_cost_ms > 0.0 {
            self.ewma_browser_cost_ms
        } else if self.ewma_cost_ms > 0.0 {
            self.ewma_cost_ms.max(450.0)
        } else {
            550.0
        };
        match strategy {
            AcquisitionStrategy::ConditionalHttp => {
                let p_304 = if self.conditional_hit.trials() >= 1.0 {
                    self.conditional_hit.mean().clamp(0.10, 0.95)
                } else {
                    0.50
                };
                p_304 * c_304 + (1.0 - p_304) * c_http
            }
            AcquisitionStrategy::LightweightHttp | AcquisitionStrategy::HttpWithEmbeddedJson => {
                c_http.max(15.0)
            }
            AcquisitionStrategy::HttpVerifyBrowser => {
                let p_ok = if self.http_sufficient.trials() >= 1.0 {
                    self.http_sufficient.mean()
                } else {
                    0.50
                };
                c_http + (1.0 - p_ok) * c_browser
            }
            AcquisitionStrategy::DirectBrowser => c_browser.max(250.0),
            AcquisitionStrategy::DeferOrSkip => 0.0,
        }
    }

    /// 建议的下次检查间隔（秒），优先使用经非混叠验证的周期估计，否则按泊松后验漏检上界计算。
    pub fn next_check_secs(&self, delta: f64) -> f64 {
        if let Some(delay) = self.substantive_change.regularity.next_delay_secs(
            self.last_observed_at_ms.max(
                self.substantive_change
                    .regularity
                    .last_change_at_ms
                    .unwrap_or(0),
            ),
            1.0,
            365.0 * 86_400.0,
        ) {
            return delay;
        }
        self.substantive_change.to_change_rate().next_check(delta)
    }

    /// 实质内容变化的后验平均速率（每秒）。
    pub fn substantive_change_rate_per_sec(&self) -> f64 {
        self.substantive_change.to_change_rate().mean()
    }
}

/// 学到的 URL 改写规则类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleKind {
    DropParam { name: String },
    DropParamValue { name: String, value: String },
    StripPathSuffix { suffix: String },
}

/// URL 改写规则生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleLifecycleState {
    Candidate,
    Verified,
    Revoked,
}

/// 可持久化、可证伪的站点级 URL 规范化改写规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedUrlRule {
    pub host: String,
    pub kind: RuleKind,
    pub state: RuleLifecycleState,
    pub confirmations: u32,
    pub falsifications: u32,
    pub updated_at_ms: u64,
}

impl LearnedUrlRule {
    pub fn new(host: impl Into<String>, kind: RuleKind, now_ms: u64) -> Self {
        Self {
            host: host.into(),
            kind,
            state: RuleLifecycleState::Candidate,
            confirmations: 0,
            falsifications: 0,
            updated_at_ms: now_ms,
        }
    }

    pub fn confirm(&mut self, min_confirmations: u32, now_ms: u64) {
        self.confirmations = self.confirmations.saturating_add(1);
        self.updated_at_ms = self.updated_at_ms.max(now_ms);
        if self.state != RuleLifecycleState::Revoked
            && self.falsifications == 0
            && self.confirmations >= min_confirmations.max(1)
        {
            self.state = RuleLifecycleState::Verified;
        }
    }

    /// 一旦出现反例（改写后 404 或正文实质不同），立即撤销该规则，防止误删有效路由参数。
    pub fn falsify(&mut self, now_ms: u64) {
        self.falsifications = self.falsifications.saturating_add(1);
        self.updated_at_ms = self.updated_at_ms.max(now_ms);
        self.state = RuleLifecycleState::Revoked;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExperienceSnapshot {
    pub families: Vec<FamilyStats>,
    pub url_rules: Vec<LearnedUrlRule>,
}

/// 线程安全的跨周期路径族经验与 URL 改写规则表。
pub struct FamilyExperienceTable {
    families: Mutex<HashMap<u64, FamilyStats>>,
    rules: Mutex<HashMap<(String, RuleKind), LearnedUrlRule>>,
}

impl Default for FamilyExperienceTable {
    fn default() -> Self {
        Self {
            families: Mutex::new(HashMap::new()),
            rules: Mutex::new(HashMap::new()),
        }
    }
}

impl FamilyExperienceTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_observation(&self, family: &str, obs: &FamilyObservation) {
        self.record_observation_for_tenant(family, None, obs);
    }

    /// 带租户边界的跨周期观测写入；若为冷启动族且同租户存在成熟相似族，先做受控 Empirical Bayes 迁移。
    pub fn record_observation_for_tenant(
        &self,
        family: &str,
        tenant_id: Option<&str>,
        obs: &FamilyObservation,
    ) {
        if !obs.has_signal() {
            return;
        }
        let key = family_key(family);
        let mut map = self.families.lock().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(&key) {
            let peers: Vec<&FamilyStats> = map.values().collect();
            let mut initial = experience_transfer::build_cold_start_seed(family, tenant_id, &peers)
                .unwrap_or_else(|| {
                    let mut f = FamilyStats::new(family);
                    f.transfer.tenant_id = tenant_id.map(str::to_string);
                    f
                });
            initial.observe(obs);
            map.insert(key, initial);
            return;
        }
        let stats = map.entry(key).or_insert_with(|| FamilyStats::new(family));
        if stats.transfer.tenant_id.is_none() && tenant_id.is_some() {
            stats.transfer.tenant_id = tenant_id.map(str::to_string);
        }
        stats.observe(obs);
    }

    /// 仅从同租户外部参考族（如同源历史源）为尚无本地观测的冷启动族接种受控先验；跨租户调用自动静默忽略。
    pub fn ensure_cold_start_seeded_for_tenant(
        &self,
        family: &str,
        tenant_id: Option<&str>,
        external_peers: &[FamilyStats],
    ) -> bool {
        let key = family_key(family);
        let mut map = self.families.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(&key)
            && existing.total_observations > 0
        {
            return false;
        }
        let local_peers: Vec<&FamilyStats> = map.values().collect();
        let ext_peers: Vec<&FamilyStats> = external_peers.iter().collect();
        if let Some(seeded) = experience_transfer::build_cold_start_seed_with_sources(
            family,
            tenant_id,
            &local_peers,
            &ext_peers,
        ) {
            map.insert(key, seeded);
            return true;
        }
        false
    }

    pub fn get_family(&self, family: &str) -> Option<FamilyStats> {
        let key = family_key(family);
        let map = self.families.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&key).cloned()
    }

    pub fn list_families(&self) -> Vec<FamilyStats> {
        let map = self.families.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<FamilyStats> = map.values().cloned().collect();
        out.sort_by(|a, b| a.family.cmp(&b.family));
        out
    }

    pub fn confirm_url_rule(&self, host: &str, kind: RuleKind, now_ms: u64) {
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let entry = rules
            .entry((host.to_ascii_lowercase(), kind.clone()))
            .or_insert_with(|| LearnedUrlRule::new(host.to_ascii_lowercase(), kind, now_ms));
        entry.confirm(DEFAULT_MIN_RULE_CONFIRMATIONS, now_ms);
    }

    pub fn falsify_url_rule(&self, host: &str, kind: &RuleKind, now_ms: u64) {
        let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let entry = rules
            .entry((host.to_ascii_lowercase(), kind.clone()))
            .or_insert_with(|| {
                LearnedUrlRule::new(host.to_ascii_lowercase(), kind.clone(), now_ms)
            });
        entry.falsify(now_ms);
    }

    pub fn list_url_rules(&self, host: Option<&str>) -> Vec<LearnedUrlRule> {
        let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
        let host_norm = host.map(str::to_ascii_lowercase);
        let mut out: Vec<LearnedUrlRule> = rules
            .values()
            .filter(|r| host_norm.as_ref().is_none_or(|h| &r.host == h))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.host.cmp(&b.host).then_with(|| a.kind.cmp(&b.kind)));
        out
    }

    /// 对 URL 应用当前主机已验证（`Verified`）且未撤销的改写规则，返回本次实际命中的规则列表。
    pub fn rewrite_url(&self, u: &mut Url) -> Vec<RuleKind> {
        let Some(host) = u.host_str().map(str::to_ascii_lowercase) else {
            return Vec::new();
        };
        let active: Vec<RuleKind> = {
            let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
            rules
                .values()
                .filter(|r| r.host == host && r.state == RuleLifecycleState::Verified)
                .map(|r| r.kind.clone())
                .collect()
        };
        if active.is_empty() {
            return Vec::new();
        }
        let mut applied = Vec::new();
        let mut path = u.path().to_string();
        for rule in &active {
            if let RuleKind::StripPathSuffix { suffix } = rule
                && path.len() > suffix.len()
                && path.ends_with(suffix.as_str())
                && path[..path.len() - suffix.len()].ends_with('/')
            {
                path.truncate(path.len() - suffix.len());
                applied.push(rule.clone());
            }
        }
        if path != u.path() {
            u.set_path(&path);
        }
        if u.query().is_some() {
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let mut kept = Vec::with_capacity(pairs.len());
            for (name, val) in pairs {
                let mut dropped = false;
                for rule in &active {
                    match rule {
                        RuleKind::DropParam { name: rn } if rn == &name => {
                            applied.push(rule.clone());
                            dropped = true;
                            break;
                        }
                        RuleKind::DropParamValue {
                            name: rn,
                            value: rv,
                        } if rn == &name && rv == &val => {
                            applied.push(rule.clone());
                            dropped = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !dropped {
                    kept.push((name, val));
                }
            }
            if !applied.is_empty() {
                if kept.is_empty() {
                    u.set_query(None);
                } else {
                    u.query_pairs_mut()
                        .clear()
                        .extend_pairs(kept.iter().map(|(k, v)| (k.as_str(), v.as_str())));
                }
            }
        }
        applied
    }

    pub fn encode_json(&self) -> Vec<u8> {
        let snap = ExperienceSnapshot {
            families: self.list_families(),
            url_rules: self.list_url_rules(None),
        };
        serde_json::to_vec(&snap).unwrap_or_default()
    }

    pub fn decode_merge_json(&self, bytes: &[u8]) -> bool {
        let Ok(snap) = serde_json::from_slice::<ExperienceSnapshot>(bytes) else {
            return false;
        };
        {
            let mut map = self.families.lock().unwrap_or_else(|e| e.into_inner());
            for fam in snap.families {
                let key = family_key(&fam.family);
                map.entry(key)
                    .and_modify(|existing| existing.merge(&fam))
                    .or_insert(fam);
            }
        }
        {
            let mut rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
            for r in snap.url_rules {
                rules
                    .entry((r.host.clone(), r.kind.clone()))
                    .and_modify(|existing| {
                        existing.confirmations = existing.confirmations.max(r.confirmations);
                        existing.falsifications = existing.falsifications.max(r.falsifications);
                        existing.updated_at_ms = existing.updated_at_ms.max(r.updated_at_ms);
                        if existing.falsifications > 0 || r.state == RuleLifecycleState::Revoked {
                            existing.state = RuleLifecycleState::Revoked;
                        } else if r.state == RuleLifecycleState::Verified {
                            existing.state = RuleLifecycleState::Verified;
                        }
                    })
                    .or_insert(r);
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "experience_tests.rs"]
mod tests;
