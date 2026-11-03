//! Planner 与 Verifier 的追加判定。
//!
//! 选计划：`π* = argmax_π E[质量增益 | 特征, 族] − Σ_r λ_r·成本_r(π)`，用 Thompson 采样从各计划的后验抽样
//! （Russo 等, FnT ML 2018, §3）。后验由调用方按路径族→站点→全局收缩后给出（层级收缩，`pagesift_stat::shrink`）。
//! 追加执行器：`(1 − Q)·E[增益 | e, 族] > λ·成本(e)`，`Q` 是当前候选的质量分；每页最多一轮。

use pagesift_stat::beta::Posterior;
use pagesift_stat::decide::{Cost, ShadowPrices, Why, choose};
use pagesift_stat::rng::Rng;

/// 候选计划。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Plan {
    Http,
    EmbeddedJson,
    HttpVerifyBrowser,
    Lightpanda,
    FullBrowser,
}

impl Plan {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::EmbeddedJson => "embedded-json",
            Self::HttpVerifyBrowser => "http+verify(browser)",
            Self::Lightpanda => "lightpanda",
            Self::FullBrowser => "full-browser",
        }
    }

    pub fn uses_browser(self) -> bool {
        matches!(
            self,
            Self::HttpVerifyBrowser | Self::Lightpanda | Self::FullBrowser
        )
    }
}

/// 一个计划的选项：后验（"这个计划给出完整结果"）、价值、预计成本。
#[derive(Debug, Clone)]
pub struct Option_ {
    pub plan: Plan,
    pub post: Posterior,
    pub value: f64,
    pub cost: Cost,
}

/// 调用方的约束。
#[derive(Debug, Clone, Copy, Default)]
pub struct Constraints {
    /// 请求显式给了 `engine: http`：不选用浏览器的计划。
    pub http_only: bool,
    /// 请求显式给了 `engine: browser`：只选用浏览器的计划。
    pub browser_only: bool,
    /// 页面是拦截页：如实报告，不升级到浏览器。
    pub barrier: bool,
}

/// 按 Thompson 采样选计划；约束排除了全部选项时返回 `None`。
pub fn choose_plan(
    options: &[Option_],
    limits: Constraints,
    prices: &ShadowPrices,
    rng: &mut Rng,
) -> Option<Plan> {
    let allowed: Vec<&Option_> = options
        .iter()
        .filter(|o| {
            let browser = o.plan.uses_browser();
            !(limits.http_only && browser)
                && !(limits.browser_only && !browser)
                && !(limits.barrier && browser)
        })
        .collect();
    let arms: Vec<_> = allowed.iter().map(|o| (o.post, o.value, o.cost)).collect();
    choose(&arms, prices, rng).map(|i| allowed[i].plan)
}

/// 是否追加执行器 `e` 做校验：`(1 − Q)·p·价值 > Σ λ·成本`，`p` 从"`e` 有增益"的后验抽样。
pub fn should_verify(
    quality: f64,
    gain: &Posterior,
    value: f64,
    cost: &Cost,
    prices: &ShadowPrices,
    rng: &mut Rng,
) -> (bool, Why) {
    pagesift_stat::decide::decide(
        gain,
        (1.0 - quality.clamp(0.0, 1.0)) * value,
        cost,
        prices,
        rng,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::beta::{BetaCounts, Prior};
    use pagesift_stat::decide::Resource;

    fn post(success: u32, failure: u32) -> Posterior {
        let mut c = BetaCounts::default();
        c.observe_weighted(success.into(), failure.into());
        c.posterior(Prior::UNIFORM)
    }

    #[test]
    fn respects_explicit_engine_and_barriers() {
        let opts = [
            Option_ {
                plan: Plan::Http,
                post: post(1, 9),
                value: 1.0,
                cost: Cost::default(),
            },
            Option_ {
                plan: Plan::FullBrowser,
                post: post(9, 1),
                value: 1.0,
                cost: Cost::default(),
            },
        ];
        let mut rng = Rng::seed(7);
        let prices = ShadowPrices::default();
        let http_only = Constraints {
            http_only: true,
            ..Default::default()
        };
        assert_eq!(
            choose_plan(&opts, http_only, &prices, &mut rng),
            Some(Plan::Http)
        );
        let barrier = Constraints {
            barrier: true,
            ..Default::default()
        };
        assert_eq!(
            choose_plan(&opts, barrier, &prices, &mut rng),
            Some(Plan::Http)
        );
        let both = Constraints {
            http_only: true,
            browser_only: true,
            ..Default::default()
        };
        assert_eq!(choose_plan(&opts, both, &prices, &mut rng), None);
    }

    #[test]
    fn verification_stops_when_quality_is_high_or_the_browser_is_expensive() {
        let gain = post(50, 50);
        let cost = Cost::default().with(Resource::Browser, 1.0);
        let mut rng = Rng::seed(3);
        let idle = ShadowPrices::default();
        assert!(should_verify(0.2, &gain, 1.0, &cost, &idle, &mut rng).0);
        assert!(
            !should_verify(
                1.0,
                &gain,
                1.0,
                &cost,
                &idle.with(Resource::Browser, 0.01),
                &mut rng
            )
            .0
        );
        let busy = ShadowPrices::default().with(Resource::Browser, f64::INFINITY);
        assert!(!should_verify(0.2, &gain, 1.0, &cost, &busy, &mut rng).0);
    }
}
