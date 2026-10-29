//! 自动浏览器升级的在线统计。
//!
//! 预算、导航耐心和早停时机都从最近实际观测到的分布推导，不写死秒数：
//! - 导航耐心 = 成功导航耗时的均值 + 若干标准差上界；
//! - 早停时机 = 页面第一次出现“比 HTTP 更多正文”的耗时上界；
//! - 渲染预算 = 真正带来增益的渲染总耗时上界。
//!
//! 样本还不足以让均值标准误落到均值的 10% 以内时，不启用对应的上限，
//! 先按原有流程跑；样本够了自动收紧。这样冷启动不会因为一条拍脑袋的固定值
//! 误杀慢站点，稳定后又不会让个别异常页面拖住整个批次。

use std::sync::Mutex;
use std::time::Duration;

/// 单侧约 99.7% 的正态上界。这是统计置信参数，不是固定超时。
const SIGMA: f64 = 3.0;
/// 均值标准误超过均值的这个比例时，认为样本还不够，暂不使用该分布。
const MAX_RELATIVE_ERROR: f64 = 0.10;

#[derive(Default, Clone, Copy)]
struct Moments {
    n: u64,
    mean: f64,
    m2: f64,
}

impl Moments {
    fn add(&mut self, value: Duration) {
        let x = value.as_secs_f64();
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        self.m2 += delta * (x - self.mean);
    }

    /// 样本足够时返回均值 + [`SIGMA`] 个标准差的耗时上界。
    fn upper(&self) -> Option<Duration> {
        if self.n < 2 {
            return None;
        }
        let variance = self.m2 / (self.n - 1) as f64;
        let stddev = variance.max(0.0).sqrt();
        let stderr = stddev / (self.n as f64).sqrt();
        if self.mean <= 0.0 || stderr > self.mean * MAX_RELATIVE_ERROR {
            return None;
        }
        Some(Duration::from_secs_f64(
            (self.mean + SIGMA * stddev).max(0.0),
        ))
    }
}

#[derive(Default)]
struct Inner {
    /// 成功完成导航的耗时（没等到 DOMContentLoaded 的页面不进这里）。
    navigation: Moments,
    /// 网络静默/正文稳定等待的耗时。
    wait: Moments,
    /// 真正比 HTTP 多出正文的渲染总耗时。
    useful_total: Moments,
    /// 没有带来增益的渲染总耗时，用来了解“无效渲染通常多久结束”。
    useless_total: Moments,
    /// 从开始等待就绪到第一次看到比 HTTP 更多正文的耗时。
    first_content: Moments,
    useful: u64,
    useless: u64,
}

/// 所有抓取共享的一份渲染统计。锁只在记录/读取几个 `f64`，不在渲染期间持有。
#[derive(Default)]
pub struct RenderStats {
    inner: Mutex<Inner>,
}

impl RenderStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次完成的浏览器渲染。`first_content` 只在页面文字确实超过 HTTP 正文时才有值。
    pub fn note_render(
        &self,
        navigation: Duration,
        wait: Duration,
        total: Duration,
        first_content: Option<Duration>,
        gained: bool,
    ) {
        let mut inner = self.inner.lock().expect("render stats poisoned");
        inner.navigation.add(navigation);
        inner.wait.add(wait);
        if gained {
            inner.useful += 1;
            inner.useful_total.add(total);
            if let Some(t) = first_content {
                inner.first_content.add(t);
            }
        } else {
            inner.useless += 1;
            inner.useless_total.add(total);
        }
    }

    /// 自动升级等主框架导航提交的上限。样本不足时返回 `None`，按完整超时等。
    pub fn nav_patience(&self) -> Option<Duration> {
        self.inner
            .lock()
            .expect("render stats poisoned")
            .navigation
            .upper()
    }

    /// 页面文字连续变化时，最晚什么时候抓 DOM 做一次“是否还有增益”的探针。
    /// 样本不足时返回 `None`，只在网络静默时探针。
    pub fn early_stop_after(&self) -> Option<Duration> {
        self.inner
            .lock()
            .expect("render stats poisoned")
            .first_content
            .upper()
    }

    /// 自动升级的安全预算：正常增益渲染的耗时上界。样本不足时返回 `None`。
    pub fn budget(&self) -> Option<Duration> {
        let inner = self.inner.lock().expect("render stats poisoned");
        inner
            .useful_total
            .upper()
            .or_else(|| inner.useless_total.upper())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_needs_more_than_one_sample() {
        let mut m = Moments::default();
        m.add(Duration::from_millis(100));
        assert!(m.upper().is_none());
    }

    #[test]
    fn upper_tracks_the_observed_distribution() {
        let mut m = Moments::default();
        for i in 0..50 {
            m.add(Duration::from_millis(100 + i));
        }
        let upper = m.upper().expect("stable samples should enable the bound");
        assert!(upper >= Duration::from_millis(149));
        assert!(upper < Duration::from_millis(500));
    }

    #[test]
    fn stats_stay_disabled_until_samples_are_stable() {
        let stats = RenderStats::new();
        assert!(stats.nav_patience().is_none());
        assert!(stats.early_stop_after().is_none());
        assert!(stats.budget().is_none());
        for i in 0..40 {
            stats.note_render(
                Duration::from_millis(50 + i),
                Duration::from_millis(100 + i),
                Duration::from_millis(200 + i),
                Some(Duration::from_millis(20 + i)),
                true,
            );
        }
        assert!(stats.nav_patience().is_some());
        assert!(stats.early_stop_after().is_some());
        assert!(stats.budget().is_some());
    }

    #[test]
    fn useless_renders_fall_back_to_the_observed_upper_bound() {
        let stats = RenderStats::new();
        for _ in 0..40 {
            stats.note_render(
                Duration::from_millis(10),
                Duration::from_millis(10),
                Duration::from_millis(20),
                None,
                false,
            );
        }
        assert!(stats.budget().is_some());
        assert!(stats.early_stop_after().is_none());
    }
}
