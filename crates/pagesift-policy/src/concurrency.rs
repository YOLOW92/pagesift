//! 全局在途数的接线：USL 目标，完成事件驱动。
//!
//! 每个名额归还时只做三次原子加法（完成数、占用时长、是否用满）；攒够一个观测窗
//! （`⌈目标⌉` 个完成：在途集合整体周转一次，Little 定律 `L = λW`）后，由恰好拿到锁的那个完成
//! （`try_lock`，拿不到就跳过，热路径不等锁）算出窗内吞吐 `X = 完成数 / 时长` 和平均在途数
//! `N̄ = Σ占用时长 / 时长`（Little 定律），交给 [`UslLimit`]。取代 0.81 的每秒采样一次
//! （`SAMPLE_INTERVAL 1 s`）和 AIMD 控制器。
//!
//! 往上探的余量 = min(用户上限 − 目标, 内存预算允许的在途数 − 目标, 文件描述符余量)，
//! 见 [`pagesift_net::resources`]。同一时刻刷新内存预算，单页 body 上限随之更新。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pagesift_net::{WaitPrice, resources};
use pagesift_stat::Delta;
use pagesift_stat::limit::UslLimit;
use pagesift_stat::usl::UslFit;
use tokio::time::Instant;

use crate::slots::Slots;

#[derive(Debug)]
struct Window {
    usl: UslLimit,
    started: Instant,
}

/// 全局并发名额和它的控制器。
#[derive(Debug)]
pub(crate) struct GlobalCtl {
    pub(crate) slots: Slots,
    /// 用户写明的上限（硬上限）。
    ceiling: Option<usize>,
    done: AtomicU64,
    busy_ns: AtomicU64,
    saturated: AtomicBool,
    /// 最近一个观测窗里名额的平均占用时长（纳秒，0 表示还没有）：准入器算影子价格用。
    service_ns: AtomicU64,
    /// 网络层"继续等待"的影子价格（见 [`wait_price`]），每个观测窗发布一次。
    price: OnceLock<Arc<WaitPrice>>,
    window: Mutex<Window>,
}

/// 继续等待一秒的影子价格（每秒）：`ρ/((1−ρ)·W)`。
///
/// 推导：名额的影子价格 `λ = W/(1−ρ)`（[`pagesift_stat::decide::shadow_price`]，
/// M/M/c 近似）里，`W` 是请求本身的服务时间，`W·ρ/(1−ρ)` 是排队造成的等待。多占名额一秒，
/// 让排队的请求总共多等 `ρ/(1−ρ)` 秒；一个请求完成的价值按"再抓一页要花的时间" `W` 计，
/// 于是继续等待规则 `h(t,Δ)·价值 > λ·Δ·占用` 里的 `λ·占用/价值 = ρ/((1−ρ)·W)`
/// （= `(shadow_price(ρ,W) − W)/W²`）。空闲时为 0，各阶段超时就等于配置的上限（同 0.81）；
/// 饱和时趋于无穷，放弃时刻按完成时间分布提前。
/// 改用：协调方给的"价格 = shadow_price(利用率, 服务时间)"在空闲时等于 `W` 而不是 0，量纲是秒而
/// 等待规则要每秒，所以只取排队部分并除以价值。
pub fn wait_price(utilization: f64, service: f64) -> f64 {
    if utilization.is_nan() || utilization <= 0.0 || service.is_nan() || service <= 0.0 {
        return 0.0;
    }
    if utilization >= 1.0 {
        return f64::INFINITY;
    }
    utilization / ((1.0 - utilization) * service)
}

impl GlobalCtl {
    /// `prior` 是冷启动的目标（先验包 v0：0.81 的现值 64，见 `RateLimitConfig`）。
    pub(crate) fn new(prior: usize, ceiling: Option<usize>) -> Self {
        let start = ceiling.map_or(prior, |c| prior.min(c)).max(1);
        resources::refresh(start);
        Self {
            slots: Slots::new(start),
            ceiling,
            done: AtomicU64::new(0),
            busy_ns: AtomicU64::new(0),
            saturated: AtomicBool::new(false),
            service_ns: AtomicU64::new(0),
            price: OnceLock::new(),
            window: Mutex::new(Window {
                usl: UslLimit::new(start as f64, Delta::DEFAULT),
                started: Instant::now(),
            }),
        }
    }

    /// 把等待价格发布到网络层（只接一次）。
    pub(crate) fn publish_wait_price(&self, price: Arc<WaitPrice>) {
        let _ = self.price.set(price);
    }

    /// 最近一个观测窗里名额的平均占用时长。
    pub(crate) fn service_time(&self) -> Option<Duration> {
        match self.service_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(Duration::from_nanos(ns)),
        }
    }

    pub(crate) fn fit(&self) -> Option<UslFit> {
        self.window.lock().ok()?.usl.fit()
    }

    /// 一个名额归还：它占用了 `held`，拿到时名额是否已用满。
    pub(crate) fn complete(&self, held: Duration, saturated: bool) {
        let ns = u64::try_from(held.as_nanos()).unwrap_or(u64::MAX);
        self.busy_ns.fetch_add(ns, Ordering::Relaxed);
        if saturated {
            self.saturated.store(true, Ordering::Relaxed);
        }
        let k = self.done.fetch_add(1, Ordering::AcqRel) + 1;
        if k < self.slots.limit() as u64 {
            return;
        }
        let Ok(mut w) = self.window.try_lock() else {
            return;
        };
        let elapsed = w.started.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        let k = self.done.swap(0, Ordering::AcqRel);
        let busy = self.busy_ns.swap(0, Ordering::AcqRel) as f64 / 1e9;
        let sat = self.saturated.swap(false, Ordering::AcqRel);
        w.started = Instant::now();
        if k == 0 {
            return;
        }
        let x = k as f64 / elapsed;
        let n_bar = busy / elapsed;
        let service = busy / k as f64;
        self.service_ns
            .store(((service * 1e9) as u64).max(1), Ordering::Relaxed);
        if let Some(p) = self.price.get() {
            // 利用率 = 窗内平均在途数 / 名额数
            p.set(wait_price(n_bar / self.slots.limit() as f64, service));
        }
        let g = w.usl.limit();
        resources::refresh(g.round() as usize);
        let user_room = self.ceiling.map_or(f64::INFINITY, |c| c as f64 - g);
        let room = user_room
            .min(resources::memory_room(g))
            .min(resources::fd_room(n_bar));
        let mut next = w.usl.window(n_bar, x, sat, room);
        if let Some(c) = self.ceiling {
            next = next.min(c);
        }
        drop(w);
        if next != self.slots.limit() {
            self.slots.set_limit(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cold_start_is_the_prior_and_the_ceiling_is_hard() {
        let g = GlobalCtl::new(64, None);
        assert_eq!(g.slots.limit(), 64);
        let g = GlobalCtl::new(64, Some(8));
        assert_eq!(g.slots.limit(), 8);
        // 一直用满、吞吐随在途数线性增长：往上探，但不超过用户上限
        for _ in 0..50 {
            tokio::time::advance(Duration::from_millis(100)).await;
            for _ in 0..g.slots.limit() {
                g.complete(Duration::from_millis(100), true);
            }
        }
        assert_eq!(g.slots.limit(), 8);
    }

    #[test]
    fn wait_price_is_zero_when_idle_and_rises_with_load() {
        assert_eq!(wait_price(0.0, 0.2), 0.0, "空闲：超时等于配置值");
        let p = [0.2, 0.5, 0.8, 0.95].map(|rho| wait_price(rho, 0.2));
        assert!(p.windows(2).all(|w| w[0] < w[1]), "{p:?}");
        assert!((p[1] - 5.0).abs() < 1e-9, "ρ=½、W=0.2 s：1/0.2 = 5 /s");
        assert!(wait_price(1.0, 0.2).is_infinite());
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_price_is_published_per_window() {
        let g = GlobalCtl::new(4, None);
        let price = Arc::new(WaitPrice::new());
        g.publish_wait_price(price.clone());
        // 窗内 4 个完成、各占 100 ms、窗长 400 ms：平均在途 1/4 名额 → 价格 (¼/¾)/0.1 ≈ 3.3 /s
        tokio::time::advance(Duration::from_millis(400)).await;
        for _ in 0..4 {
            g.complete(Duration::from_millis(100), false);
        }
        let light = price.get();
        assert!((light - 10.0 / 3.0).abs() < 1e-6, "{light}");
        // 负载升高（窗长 110 ms 内同样的占用：利用率 ≈ 0.91）：价格上升，放弃时刻随之提前
        // （放弃时刻随价格单调提前见 pagesift-net 的 wait::rule 测试）
        tokio::time::advance(Duration::from_millis(110)).await;
        for _ in 0..g.slots.limit() {
            g.complete(Duration::from_millis(100), false);
        }
        assert!(price.get() > light, "{} vs {light}", price.get());
    }

    #[tokio::test(start_paused = true)]
    async fn saturated_linear_scaling_probes_upward_without_demand_it_holds() {
        let g = GlobalCtl::new(16, None);
        for _ in 0..5 {
            tokio::time::advance(Duration::from_millis(100)).await;
            for _ in 0..g.slots.limit() {
                g.complete(Duration::from_millis(100), false);
            }
        }
        assert_eq!(g.slots.limit(), 16, "需求不够时不动");
        for _ in 0..5 {
            tokio::time::advance(Duration::from_millis(100)).await;
            for _ in 0..g.slots.limit() {
                g.complete(Duration::from_millis(100), true);
            }
        }
        assert!(g.slots.limit() > 16);
    }
}
