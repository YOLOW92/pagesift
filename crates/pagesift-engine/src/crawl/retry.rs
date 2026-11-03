//! 临时性失败的重试：到点才重新调度的队列和每次的等待时长。

use std::collections::HashMap;

use super::*;

/// 等着重试的页面：到点了才重新调度。
///
/// 要不要重试：每类失败（状态码或错误码）记下
/// 重试的结果，成功率的 Wilson 上界（单侧 `z(1−δ)`，Wilson, JASA 22, 1927）不到 `δ` 时，
/// 这类失败就不再重试——有把握重试几乎从不成功（死链、永久性 5xx 的站点）。没有数据时上界是 1，
/// 行为和 0.81 一样。次数上限 [`MAX_RETRIES`]、[`SEED_MAX_RETRIES`] 留作安全上限。
/// 等待间隔保留 0.81 的指数退避（Retry-After 优先，RFC 9110 §10.2.3）：恢复时间只能在我们自己
/// 选的重试时刻观察到（区间截尾），按它估出的 KM 分布只会复现我们自己的退避表，推不出更好的间隔。
#[derive(Default)]
pub(super) struct Retries {
    /// (到点时间, 地址, 深度, 下一次是第几次, 上一次的错误)
    items: Vec<(tokio::time::Instant, Url, usize, u32, Option<ApiError>)>,
    /// 退避开关关掉时（`[policy] backoff = false`）重试不等待，Retry-After 也不管。
    immediate: bool,
    /// 等着重试的地址 → 它上一次失败的类别。
    pending: HashMap<String, String>,
    /// 每类失败的重试结果。
    outcomes: HashMap<String, pagesift_stat::beta::BetaCounts>,
}

impl Retries {
    /// `backoff` 是策略的退避开关。
    pub(super) fn new(backoff: bool) -> Self {
        Self {
            immediate: !backoff,
            ..Self::default()
        }
    }

    /// 这类失败的重试还值得做：重试成功率的 Wilson 上界不低于 `δ`。
    fn worth_retrying(&self, class: &str) -> bool {
        let delta = pagesift_stat::Delta::DEFAULT;
        self.outcomes.get(class).is_none_or(|c| {
            pagesift_stat::beta::wilson(c.success, c.trials(), delta).1 >= delta.get()
        })
    }

    /// 第 `attempt` 次失败的结果值得重试就排进来，返回 true；否则返回 false，结果照常交付。
    /// `seed` 说它是不是种子页（重试得多、等得久）。
    pub(super) fn offer(
        &mut self,
        result: &Result<pagesift_api::types::Document, ApiError>,
        url: &Url,
        depth: usize,
        attempt: u32,
        seed: bool,
    ) -> bool {
        let (max, base) = if seed {
            (SEED_MAX_RETRIES, SEED_RETRY_BASE)
        } else {
            (MAX_RETRIES, RETRY_BASE)
        };
        let wait = retry_wait(result, attempt, base)
            .map(|w| if self.immediate { Duration::ZERO } else { w });
        // 这是一次重试的结果：记到上一次失败的类别上（不再需要重试 = 重试有用）
        if let Some(prev) = self.pending.remove(url.as_str()) {
            self.outcomes
                .entry(prev)
                .or_default()
                .observe(wait.is_none());
        }
        let class = failure_class(result);
        match wait {
            Some(wait)
                if attempt < max
                    && wait <= MAX_WAIT
                    && class.as_deref().is_none_or(|c| self.worth_retrying(c)) =>
            {
                if let Some(c) = class {
                    self.pending.insert(url.to_string(), c);
                }
                self.items.push((
                    tokio::time::Instant::now() + wait,
                    url.clone(),
                    depth,
                    attempt + 1,
                    result.as_ref().err().cloned(),
                ));
                true
            }
            _ => false,
        }
    }

    /// 已经到点的一个（最早的）。
    pub(super) fn due(&mut self) -> Option<(Url, usize, u32)> {
        let now = tokio::time::Instant::now();
        let i = (0..self.items.len())
            .filter(|&i| self.items[i].0 <= now)
            .min_by_key(|&i| self.items[i].0)?;
        let (_, u, d, a, _) = self.items.swap_remove(i);
        Some((u, d, a))
    }

    /// 取出这个主机上所有等着重试的页面和它们上一次的错误（主机已经停掉，不再重试）。
    pub(super) fn drop_host(&mut self, host: &str) -> Vec<(Url, Option<ApiError>)> {
        let mut out = Vec::new();
        self.items.retain(|(_, u, _, _, e)| {
            let keep = u.host_str() != Some(host);
            if !keep {
                out.push((u.clone(), e.clone()));
            }
            keep
        });
        for (u, _) in &out {
            self.pending.remove(u.as_str());
        }
        out
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn next_due(&self) -> Option<tokio::time::Instant> {
        self.items.iter().map(|r| r.0).min()
    }
}

/// 失败的类别：HTTP 状态码或错误码。成功的结果没有类别。
fn failure_class(result: &Result<pagesift_api::types::Document, ApiError>) -> Option<String> {
    match result {
        // [协议] RFC 9110 §15：4xx、5xx 是失败
        Ok(doc) if doc.metadata.status_code >= 400 => Some(doc.metadata.status_code.to_string()),
        Ok(_) => None,
        Err(e) => Some(format!("{:?}", e.code)),
    }
}

/// 这次结果值不值得重试，值得的话先等多久。
pub(super) fn retry_wait(
    result: &Result<pagesift_api::types::Document, ApiError>,
    attempt: u32,
    base: Duration,
) -> Option<Duration> {
    let backoff = base * 2u32.pow(attempt.min(6));
    match result {
        Ok(doc) => matches!(
            doc.metadata.status_code,
            408 | 429 | 500 | 502 | 503 | 504 | 522 | 524
        )
        .then_some(backoff),
        Err(e) => match e.code {
            ErrorCode::HostCooldown | ErrorCode::RateLimited => {
                Some(e.retry_after.map_or(backoff, |s| {
                    Duration::from_secs(s).max(Duration::from_millis(100))
                }))
            }
            ErrorCode::Timeout
            | ErrorCode::NetworkError
            | ErrorCode::UpstreamError
            | ErrorCode::Busy => Some(backoff),
            _ => None,
        },
    }
}
