//! 同租户跨族冷启动层次经验迁移（Empirical Bayes）与负迁移检测拒止。
//!
//! 严格按 `tenant_id` 隔离租户边界：禁止跨越不同认证租户或不同私有上下文共享先验；
//! 先验等效样本权重封顶，并在前若干次本地独立验证出现连续矛盾时回滚为纯本地后验。

use pagesift_stat::beta::{BetaCounts, Prior};
use pagesift_stat::shrink::beta_prior;
use serde::{Deserialize, Serialize};
use url::Url;

use super::FamilyStats;
use super::experience_rates::{BetaSnapshot, FamilyObservation};

const MAX_TRANSFER_PRIOR_WEIGHT: f64 = 2.5;
const MIN_MATURE_DONOR_OBS: u64 = 4;
const VALIDATION_WINDOW_STEPS: u8 = 4;

/// 路径族迁移与本地独立验证元数据。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferMeta {
    /// 隔离边界：仅允许相同 `tenant_id`（含均为空时的本地非隔离态）之间的冷启动迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// 捐赠源标识（用于可审计与可解释决策原因）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub donor_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded_http_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded_browser_mean: Option<f64>,
    #[serde(default)]
    pub validation_remaining: u8,
    #[serde(default)]
    pub contradiction_count: u8,
    #[serde(default)]
    pub rejected: bool,
    #[serde(default)]
    pub local_http_succ: u32,
    #[serde(default)]
    pub local_http_fail: u32,
    #[serde(default)]
    pub local_bg_succ: u32,
    #[serde(default)]
    pub local_bg_fail: u32,
    #[serde(default)]
    pub local_ej_succ: u32,
    #[serde(default)]
    pub local_ej_fail: u32,
}

/// 提取标准化主机与结构化路径骨架（如 `("/products/{id}", "shop.example.com")`）。
pub fn host_and_path_shape(family: &str) -> Option<(String, String)> {
    let url = Url::parse(family).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let segs: Vec<String> = url
        .path_segments()
        .map(|s| {
            s.filter(|p| !p.is_empty())
                .take(3)
                .map(generalize_segment)
                .collect()
        })
        .unwrap_or_default();
    let shape = if segs.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segs.join("/"))
    };
    Some((host, shape))
}

fn generalize_segment(seg: &str) -> String {
    if seg == "{id}" || seg == "{slug}" || seg == "{n}" {
        return seg.to_string();
    }
    if seg.chars().all(|c| c.is_ascii_digit()) {
        return "{id}".to_string();
    }
    if seg.len() >= 16 && seg.chars().filter(|c| c.is_ascii_digit()).count() >= 4 {
        return "{id}".to_string();
    }
    seg.to_ascii_lowercase()
}

fn shrink_rate_prior(donors: &[BetaSnapshot]) -> Option<BetaSnapshot> {
    if donors.is_empty() {
        return None;
    }
    if donors.len() >= 2 {
        let counts: Vec<BetaCounts> = donors.iter().copied().map(BetaCounts::from).collect();
        if let Some(p) = beta_prior(&counts) {
            return Some(cap_prior(p, MAX_TRANSFER_PRIOR_WEIGHT));
        }
    }
    // 仅有单个同构型成熟捐赠者时，给一个更保守的低权重先验（<= 1.6），留出快速本地覆写余地
    let d = donors[0];
    let prior = Prior {
        alpha: d.success + 1.0,
        beta: d.failure + 1.0,
    };
    Some(cap_prior(prior, 1.6))
}

fn cap_prior(p: Prior, max_weight: f64) -> BetaSnapshot {
    let total = (p.alpha + p.beta).max(2.0);
    let mean = (p.alpha / total).clamp(0.05, 0.95);
    let eff = (total - 2.0).clamp(0.5, max_weight);
    BetaSnapshot {
        success: mean * eff,
        failure: (1.0 - mean) * eff,
    }
}

/// 在同租户前提下，从同源/同路径骨架的成熟家族计算受控冷启动先验种子。
pub fn build_cold_start_seed(
    target_family: &str,
    tenant_id: Option<&str>,
    peers: &[&FamilyStats],
) -> Option<FamilyStats> {
    build_cold_start_seed_with_sources(target_family, tenant_id, peers, &[])
}

/// 同上，并区分“本表内 sibling 族（排除同 key 自引用）”与“同租户跨数据源外部族（允许相同 path prefix 迁移）”。
pub fn build_cold_start_seed_with_sources(
    target_family: &str,
    tenant_id: Option<&str>,
    same_table_peers: &[&FamilyStats],
    external_peers: &[&FamilyStats],
) -> Option<FamilyStats> {
    let (target_host, target_shape) = host_and_path_shape(target_family)?;
    let eligible = |p: &FamilyStats, allow_same_family: bool| -> bool {
        (allow_same_family || p.family != target_family)
            && p.total_observations >= MIN_MATURE_DONOR_OBS
            && !p.transfer.rejected
            && p.transfer.tenant_id.as_deref() == tenant_id
            && host_and_path_shape(&p.family).is_some_and(|(h, _)| h == target_host)
    };
    let same_host: Vec<&FamilyStats> = same_table_peers
        .iter()
        .copied()
        .filter(|p| eligible(p, false))
        .chain(external_peers.iter().copied().filter(|p| eligible(p, true)))
        .collect();
    if same_host.is_empty() {
        return None;
    }

    let exact_ext: Vec<&FamilyStats> = external_peers
        .iter()
        .copied()
        .filter(|p| eligible(p, true) && p.family == target_family)
        .collect();
    let same_shape: Vec<&FamilyStats> = same_host
        .iter()
        .copied()
        .filter(|p| host_and_path_shape(&p.family).is_some_and(|(_, sh)| sh == target_shape))
        .collect();
    let (chosen, scope_tag) = if !exact_ext.is_empty() {
        (exact_ext, "family")
    } else if !same_shape.is_empty() {
        (same_shape, "shape")
    } else if same_host.len() >= 2 {
        (same_host, "host")
    } else {
        // 不同结构族且仅有一个同域样本时不冒险跨模板迁移
        return None;
    };

    let http_donors: Vec<BetaSnapshot> = chosen
        .iter()
        .filter(|p| p.http_sufficient.trials() >= 2.0)
        .map(|p| p.http_sufficient)
        .collect();
    let bg_donors: Vec<BetaSnapshot> = chosen
        .iter()
        .filter(|p| p.browser_gain.trials() >= 2.0)
        .map(|p| p.browser_gain)
        .collect();
    let ej_donors: Vec<BetaSnapshot> = chosen
        .iter()
        .filter(|p| p.embedded_json.trials() >= 2.0)
        .map(|p| p.embedded_json)
        .collect();
    let barrier_donors: Vec<BetaSnapshot> = chosen
        .iter()
        .filter(|p| p.barrier_rate.trials() >= 2.0)
        .map(|p| p.barrier_rate)
        .collect();

    let http_seed = shrink_rate_prior(&http_donors);
    let bg_seed = shrink_rate_prior(&bg_donors);
    let ej_seed = shrink_rate_prior(&ej_donors);
    let barrier_seed = shrink_rate_prior(&barrier_donors);
    if http_seed.is_none() && bg_seed.is_none() && ej_seed.is_none() {
        return None;
    }

    let mut seeded = FamilyStats::new(target_family);
    if let Some(h) = http_seed {
        seeded.transfer.seeded_http_mean = Some(h.mean());
        seeded.http_sufficient = h;
    }
    if let Some(b) = bg_seed {
        seeded.transfer.seeded_browser_mean = Some(b.mean());
        seeded.browser_gain = b;
    }
    if let Some(e) = ej_seed {
        seeded.embedded_json = e;
    }
    if let Some(br) = barrier_seed {
        seeded.barrier_rate = br;
    }
    seeded.transfer.tenant_id = tenant_id.map(str::to_string);
    seeded.transfer.validation_remaining = VALIDATION_WINDOW_STEPS;
    seeded.transfer.donor_description =
        Some(format!("eb_{scope_tag}:{target_host}:n={}", chosen.len()));
    Some(seeded)
}

/// 在验证窗内检验本地独立观测是否与迁移先验矛盾；若触发负迁移，清除外源先验并仅保留本地后验。
pub fn validate_or_reject_transfer(stats: &mut FamilyStats, obs: &FamilyObservation) -> bool {
    if stats.transfer.rejected
        || stats.transfer.validation_remaining == 0
        || obs.unreliable_observation
    {
        return false;
    }

    let mut recorded_local_check = false;
    let mut contradicted = false;
    if let Some(ok) = obs.http_sufficient {
        recorded_local_check = true;
        if ok {
            stats.transfer.local_http_succ = stats.transfer.local_http_succ.saturating_add(1);
        } else {
            stats.transfer.local_http_fail = stats.transfer.local_http_fail.saturating_add(1);
        }
        if let Some(seed_mean) = stats.transfer.seeded_http_mean
            && ((seed_mean >= 0.58 && !ok) || (seed_mean <= 0.42 && ok))
        {
            contradicted = true;
        }
    }
    if let Some(gain) = obs.browser_gain {
        recorded_local_check = true;
        if gain {
            stats.transfer.local_bg_succ = stats.transfer.local_bg_succ.saturating_add(1);
        } else {
            stats.transfer.local_bg_fail = stats.transfer.local_bg_fail.saturating_add(1);
        }
        if let Some(seed_mean) = stats.transfer.seeded_browser_mean
            && ((seed_mean >= 0.58 && !gain) || (seed_mean <= 0.42 && gain))
        {
            contradicted = true;
        }
    }
    if let Some(ej) = obs.embedded_json_observed {
        if ej {
            stats.transfer.local_ej_succ = stats.transfer.local_ej_succ.saturating_add(1);
        } else {
            stats.transfer.local_ej_fail = stats.transfer.local_ej_fail.saturating_add(1);
        }
    }

    if !recorded_local_check {
        return false;
    }
    if contradicted {
        stats.transfer.contradiction_count = stats.transfer.contradiction_count.saturating_add(1);
    }
    stats.transfer.validation_remaining = stats.transfer.validation_remaining.saturating_sub(1);

    if stats.transfer.contradiction_count >= 2 {
        // 负迁移触发回滚：彻底抹除捐赠先验，仅由本地记录重构干净 Beta 后验
        stats.http_sufficient = BetaSnapshot {
            success: stats.transfer.local_http_succ as f64,
            failure: stats.transfer.local_http_fail as f64,
        };
        stats.browser_gain = BetaSnapshot {
            success: stats.transfer.local_bg_succ as f64,
            failure: stats.transfer.local_bg_fail as f64,
        };
        stats.embedded_json = BetaSnapshot {
            success: stats.transfer.local_ej_succ as f64,
            failure: stats.transfer.local_ej_fail as f64,
        };
        stats.transfer.rejected = true;
        stats.transfer.validation_remaining = 0;
        stats.drift_count = stats.drift_count.saturating_add(1);
        return true;
    }
    false
}
