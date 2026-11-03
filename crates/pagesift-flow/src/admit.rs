//! 统一准入门面：CPU、网络在途、浏览器标签、内存四种资源的当前用量、上限和影子价格。
//!
//! | 资源 | 上限从哪来 | 实际执行在哪 |
//! |---|---|---|
//! | CPU | 工人数 = 允许的核数（[`crate::cpu::Pool`]） | 工人队列 |
//! | 网络在途 | USL 峰值的 `1−δ` 下界；每站点延迟梯度（[`pagesift_policy::RateLimiter`]） | 限速器名额 |
//! | 内存 | 可用内存 − 系统其余部分的 `1−δ` 预测增量（[`pagesift_net::resources`]） | 单页 body 上限、全局并发的探测余量 |
//! | 浏览器标签 | 冷启动 = 各内核标签数之和，之后由渲染路径按需调整 | [`Admission::tab`] 的名额 |
//!
//! 影子价格 `λ_r = W_r / (1 − ρ_r)`（M/M/c 近似，见 [`pagesift_stat::decide::shadow_price`]）：
//! 空闲时趋于服务时间本身，饱和时趋于无穷，供计划器在"做不做"之间权衡。
//! CPU 没有直接的利用率读数，用工人池实测的派发往返（排队 + 唤醒）作为占用一份 CPU 的边际等待。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pagesift_policy::{RateLimiter, Slot, Slots};
use pagesift_stat::Stat;
use pagesift_stat::decide::{Resource, ShadowPrices, shadow_price};
use pagesift_stat::moments::Moments;
use pagesift_stat::usl::UslFit;

use crate::cpu::Pool;

/// 某一时刻四种资源的状态。
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub cpu_workers: usize,
    /// 工人池的派发往返（还没有样本时为 `None`）。
    pub cpu_round_trip: Option<Duration>,
    pub net_limit: usize,
    pub net_active: usize,
    /// 最近一个观测窗里网络名额的平均占用时长。
    pub net_service: Option<Duration>,
    pub net_fit: Option<UslFit>,
    /// 内存预算（字节）；读不到时为 `None`。
    pub memory_budget: Option<u64>,
    pub memory_total: Option<u64>,
    pub tab_limit: usize,
    pub tab_active: usize,
    /// 标签的平均占用时长。
    pub tab_service: Option<Duration>,
}

impl Snapshot {
    /// 各资源的影子价格（秒）。没有服务时间读数的资源价格为 0（没有证据说明它稀缺）。
    pub fn prices(&self) -> ShadowPrices {
        let secs = |d: Option<Duration>| d.map_or(0.0, |d| d.as_secs_f64());
        let net_rho = self.net_active as f64 / self.net_limit.max(1) as f64;
        let tab_rho = self.tab_active as f64 / self.tab_limit.max(1) as f64;
        let mem_rho = match (self.memory_budget, self.memory_total) {
            (Some(b), Some(t)) if t > 0 => 1.0 - b as f64 / t as f64,
            _ => 0.0,
        };
        ShadowPrices::default()
            .with(Resource::Cpu, secs(self.cpu_round_trip))
            .with(
                Resource::Politeness,
                shadow_price(net_rho, secs(self.net_service)),
            )
            // 内存按字节秒计价，持有时长取网络名额的占用时长（页面在途期间占着它的 body）
            .with(
                Resource::Memory,
                shadow_price(mem_rho, secs(self.net_service)),
            )
            .with(
                Resource::Browser,
                shadow_price(tab_rho, secs(self.tab_service)),
            )
    }
}

/// 准入门面。网络名额在限速器里（每个请求都过它），这里持有浏览器标签的名额，并汇总四种资源。
#[derive(Debug)]
pub struct Admission {
    tabs: Slots,
    tab_hold: Arc<Mutex<Moments>>,
}

/// 一个浏览器标签名额；丢弃时归还并记下占用时长。
#[derive(Debug)]
pub struct TabPermit {
    _slot: Slot,
    since: Instant,
    hold: Arc<Mutex<Moments>>,
}

impl Drop for TabPermit {
    fn drop(&mut self) {
        if let Ok(mut m) = self.hold.lock() {
            m.push(self.since.elapsed().as_secs_f64());
        }
    }
}

impl Admission {
    /// `tabs` 是标签名额的冷启动值（先验包 v0：0.81 各内核"浏览器数 × 每个浏览器的标签数"之和）。
    pub fn new(tabs: usize) -> Self {
        Self {
            tabs: Slots::new(tabs),
            tab_hold: Arc::new(Mutex::new(Moments::default())),
        }
    }

    /// 排队拿一个浏览器标签名额（先进先出）。
    pub async fn tab(&self) -> TabPermit {
        TabPermit {
            _slot: self.tabs.acquire().await,
            since: Instant::now(),
            hold: self.tab_hold.clone(),
        }
    }

    /// 调整标签名额（渲染路径按内存预测或 USL 得出新值时调用）。
    pub fn set_tab_limit(&self, n: usize) {
        self.tabs.set_limit(n);
    }

    /// 当前四种资源的状态。
    pub fn snapshot(&self, net: &RateLimiter, cpu: Option<&Pool>) -> Snapshot {
        let tab_service = self
            .tab_hold
            .lock()
            .ok()
            .and_then(|m| (m.count() > 0).then(|| Duration::from_secs_f64(m.mean().max(0.0))));
        Snapshot {
            cpu_workers: cpu.map_or(0, Pool::workers),
            cpu_round_trip: cpu.and_then(Pool::round_trip),
            net_limit: net.global_limit(),
            net_active: net.global_active(),
            net_service: net.global_service_time(),
            net_fit: net.global_fit(),
            memory_budget: pagesift_net::resources::budget(),
            memory_total: pagesift_net::resources::read_memory().map(|m| m.total_bytes),
            tab_limit: self.tabs.limit(),
            tab_active: self.tabs.active(),
            tab_service,
        }
    }

    /// 浏览器标签的利用率 `ρ = 在用 / 上限`（推测执行用它定浏览器的影子价格）。
    pub fn tab_utilization(&self) -> f64 {
        self.tabs.active() as f64 / self.tabs.limit().max(1) as f64
    }

    /// 合并另一份标签占用统计（多进程汇总用）。
    pub fn absorb_tab_stats(&self, other: &Moments) {
        if let Ok(mut m) = self.tab_hold.lock() {
            m.merge(other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prices_rise_with_utilization_and_tabs_are_bounded() {
        let net = RateLimiter::new(Default::default());
        let adm = Admission::new(2);
        let a = adm.tab().await;
        let _b = adm.tab().await;
        let full = adm.snapshot(&net, None);
        assert_eq!((full.tab_limit, full.tab_active), (2, 2));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), adm.tab())
                .await
                .is_err(),
            "名额用满时排队"
        );
        drop(a);
        let snap = adm.snapshot(&net, None);
        assert!(snap.tab_service.is_some());
        let idle = Snapshot {
            tab_active: 0,
            ..snap.clone()
        };
        let busy = Snapshot {
            tab_active: 2,
            ..snap
        };
        assert!(busy.prices().get(Resource::Browser).is_infinite());
        assert!(idle.prices().get(Resource::Browser) >= 0.0);
        assert_eq!(full.net_limit, 64);
    }
}
