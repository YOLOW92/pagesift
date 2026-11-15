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
use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::poisson::ChangeRate;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::render::family_key;

const DEFAULT_MIN_RULE_CONFIRMATIONS: u32 = 2; // [推导] 对应 p₀ = √δ 下 p₀^k ≤ δ 的最小独立证实数 k* = 2

/// 单次采集对路径族经验的观测输入。
#[derive(Debug, Clone)]
pub struct FamilyObservation {
    pub observed_at_ms: u64,
    /// 轻量 HTTP（含内嵌 JSON）是否已满足本次采集目标。
    pub http_sufficient: bool,
    /// 若实际启动了浏览器，浏览器是否带来正文或结构化声明增益。
    pub browser_gain: Option<bool>,
    /// 若发送了条件请求（`If-None-Match` / `If-Modified-Since`），是否命中 304。
    pub conditional_hit: Option<bool>,
    /// 距上次观测的时间间隔（秒）及是否发生**实质内容变化**（排除纯时间/计数噪声）。
    pub substantive_change: Option<(f64, bool)>,
    /// 本次采集是否产出至少 1 条结构化声明或新发现链接。
    pub yielded_claims_or_links: bool,
    /// 本次采集是否遭遇拦截（403 / 429 / 验证码挑战）。
    pub encountered_barrier: bool,
    /// 本次采集耗时（毫秒）。
    pub cost_ms: f64,
}

/// 可序列化的 Beta 计数快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BetaSnapshot {
    pub success: f64,
    pub failure: f64,
}

impl From<BetaCounts> for BetaSnapshot {
    fn from(c: BetaCounts) -> Self {
        Self {
            success: c.success,
            failure: c.failure,
        }
    }
}

impl From<BetaSnapshot> for BetaCounts {
    fn from(s: BetaSnapshot) -> Self {
        Self {
            success: s.success.max(0.0),
            failure: s.failure.max(0.0),
        }
    }
}

impl BetaSnapshot {
    pub fn observe_decayed(&mut self, ok: bool, keep: f64) {
        self.success *= keep;
        self.failure *= keep;
        if ok {
            self.success += 1.0;
        } else {
            self.failure += 1.0;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.success += other.success;
        self.failure += other.failure;
    }

    pub fn mean(self) -> f64 {
        BetaCounts::from(self).posterior(Prior::UNIFORM).mean()
    }

    pub fn trials(self) -> f64 {
        self.success + self.failure
    }
}

/// 可序列化的泊松变化率快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRateSnapshot {
    pub shape: f64,
    pub exposure_secs: f64,
    pub changed_intervals_secs: Vec<f64>,
}

impl Default for ChangeRateSnapshot {
    fn default() -> Self {
        let cr = ChangeRate::default();
        Self {
            shape: cr.shape,
            exposure_secs: cr.exposure,
            changed_intervals_secs: cr.changed,
        }
    }
}

impl ChangeRateSnapshot {
    pub fn to_change_rate(&self) -> ChangeRate {
        ChangeRate {
            shape: self.shape.max(1e-6),
            exposure: self.exposure_secs.max(1.0),
            changed: self.changed_intervals_secs.clone(),
        }
    }

    pub fn observe(&mut self, dt_secs: f64, substantive_changed: bool) {
        if !(dt_secs > 0.0 && dt_secs.is_finite()) {
            return;
        }
        if substantive_changed {
            self.changed_intervals_secs.push(dt_secs);
            // 保留最近 32 个变化区间，防止长周期运行无限增长同时保持近期变化灵敏度
            if self.changed_intervals_secs.len() > 32 {
                let drain = self.changed_intervals_secs.len() - 32;
                self.changed_intervals_secs.drain(0..drain);
            }
        } else {
            self.exposure_secs += dt_secs;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        let default_exp = ChangeRate::default().exposure;
        let added_exp = (other.exposure_secs - default_exp).max(0.0);
        self.exposure_secs += added_exp;
        for &dt in &other.changed_intervals_secs {
            if self.changed_intervals_secs.len() < 32 {
                self.changed_intervals_secs.push(dt);
            }
        }
    }
}

/// 单个路径族的跨周期经验统计。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyStats {
    pub family: String,
    pub http_sufficient: BetaSnapshot,
    pub browser_gain: BetaSnapshot,
    pub conditional_hit: BetaSnapshot,
    pub substantive_change: ChangeRateSnapshot,
    pub claim_yield: BetaSnapshot,
    pub barrier_rate: BetaSnapshot,
    pub ewma_cost_ms: f64,
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
            substantive_change: ChangeRateSnapshot::default(),
            claim_yield: BetaSnapshot::default(),
            barrier_rate: BetaSnapshot::default(),
            ewma_cost_ms: 0.0,
            total_observations: 0,
            last_observed_at_ms: 0,
        }
    }

    pub fn observe(&mut self, obs: &FamilyObservation) {
        let keep = 1.0 - Delta::DEFAULT.get();
        self.http_sufficient
            .observe_decayed(obs.http_sufficient, keep);
        if let Some(gain) = obs.browser_gain {
            self.browser_gain.observe_decayed(gain, keep);
        }
        if let Some(hit) = obs.conditional_hit {
            self.conditional_hit.observe_decayed(hit, keep);
        }
        if let Some((dt, changed)) = obs.substantive_change {
            self.substantive_change.observe(dt, changed);
        }
        self.claim_yield
            .observe_decayed(obs.yielded_claims_or_links, keep);
        self.barrier_rate
            .observe_decayed(obs.encountered_barrier, keep);
        if obs.cost_ms >= 0.0 && obs.cost_ms.is_finite() {
            if self.total_observations == 0 {
                self.ewma_cost_ms = obs.cost_ms;
            } else {
                let alpha = Delta::DEFAULT.get();
                self.ewma_cost_ms = (1.0 - alpha) * self.ewma_cost_ms + alpha * obs.cost_ms;
            }
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
        self.substantive_change.merge(&other.substantive_change);
        self.claim_yield.merge(&other.claim_yield);
        self.barrier_rate.merge(&other.barrier_rate);
        if self.total_observations == 0 {
            self.ewma_cost_ms = other.ewma_cost_ms;
        } else if other.total_observations > 0 {
            self.ewma_cost_ms = 0.5 * (self.ewma_cost_ms + other.ewma_cost_ms);
        }
        self.total_observations = self
            .total_observations
            .saturating_add(other.total_observations);
        self.last_observed_at_ms = self.last_observed_at_ms.max(other.last_observed_at_ms);
    }

    /// 建议的下次检查间隔（秒），使漏检实质变化的后验概率不超过 `delta`。
    pub fn next_check_secs(&self, delta: f64) -> f64 {
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
        let key = family_key(family);
        let mut map = self.families.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key)
            .or_insert_with(|| FamilyStats::new(family))
            .observe(obs);
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
mod tests {
    use super::*;

    #[test]
    fn family_experience_tracks_rates_and_roundtrips_json() {
        let table = FamilyExperienceTable::new();
        let fam = "https://docs.example.com/guide";
        for i in 0..8 {
            table.record_observation(
                fam,
                &FamilyObservation {
                    observed_at_ms: 1000 + i * 1000,
                    http_sufficient: true,
                    browser_gain: Some(false),
                    conditional_hit: Some(i > 0),
                    substantive_change: Some((3600.0, false)),
                    yielded_claims_or_links: true,
                    encountered_barrier: false,
                    cost_ms: 42.0,
                },
            );
        }
        let stats = table.get_family(fam).expect("family exists");
        assert!(stats.http_sufficient.mean() > 0.8);
        assert!(stats.conditional_hit.mean() > 0.7);
        assert!(stats.barrier_rate.mean() < 0.2);

        let encoded = table.encode_json();
        let restored = FamilyExperienceTable::new();
        assert!(restored.decode_merge_json(&encoded));
        let stats2 = restored.get_family(fam).expect("restored family");
        assert_eq!(stats2.total_observations, 8);
    }

    #[test]
    fn learned_url_rule_activates_and_revokes_on_falsification() {
        let table = FamilyExperienceTable::new();
        let rule = RuleKind::DropParam {
            name: "ref".to_string(),
        };
        table.confirm_url_rule("example.com", rule.clone(), 100);
        let mut u = Url::parse("https://example.com/item?id=1&ref=nav").unwrap();
        assert!(table.rewrite_url(&mut u).is_empty());

        table.confirm_url_rule("example.com", rule.clone(), 200);
        let applied = table.rewrite_url(&mut u);
        assert_eq!(applied, vec![rule.clone()]);
        assert_eq!(u.as_str(), "https://example.com/item?id=1");

        // 证伪后立即撤销，不再改写
        table.falsify_url_rule("example.com", &rule, 300);
        let mut u2 = Url::parse("https://example.com/item?id=1&ref=important").unwrap();
        assert!(table.rewrite_url(&mut u2).is_empty());
        assert_eq!(u2.as_str(), "https://example.com/item?id=1&ref=important");
    }
}
