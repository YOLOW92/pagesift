//! 临时性失败的重试：到点才重新调度的队列和每次的等待时长。

use super::*;

/// 等着重试的页面：到点了才重新调度。
#[derive(Default)]
pub(super) struct Retries {
    /// (到点时间, 地址, 深度, 下一次是第几次, 上一次的错误)
    items: Vec<(tokio::time::Instant, Url, usize, u32, Option<ApiError>)>,
}

impl Retries {
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
        match retry_wait(result, attempt, base) {
            Some(wait) if attempt < max && wait <= MAX_WAIT => {
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
        out
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(super) fn next_due(&self) -> Option<tokio::time::Instant> {
        self.items.iter().map(|r| r.0).min()
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
