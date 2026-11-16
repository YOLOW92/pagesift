//! Tick 预算与单调墙钟截止账本（C08 / C09 / C10）。

use std::future::Future;
use std::time::Duration;

use pagesift_api::{ApiError, ErrorCode};
use pagesift_store::{MAX_REQUESTS_PER_TICK_CAP, SyncBudget};
use tokio::time::Instant;

/// 单次受预算约束执行的错误分类：明确区分 Tick 预算耗尽与目标请求失败（R04 / P1-03）。
#[derive(Debug)]
pub(super) enum TickStepError {
    BudgetExhausted,
    Request(ApiError),
}

fn checked_deadline(now: Instant, wall_ms: u64) -> Instant {
    if let Some(d) = now.checked_add(Duration::from_millis(wall_ms)) {
        return d;
    }
    for cap_ms in [315_360_000_000u64, 31_536_000_000, 86_400_000, 3_600_000] {
        if let Some(d) = now.checked_add(Duration::from_millis(wall_ms.min(cap_ms))) {
            return d;
        }
    }
    now
}

/// 同步周期硬预算账本：在每次抓取、重写回退、浏览器升级前原子扣减配额。
#[derive(Debug, Clone)]
pub struct TickBudgetLedger {
    deadline: Instant,
    pub max_requests: usize,
    pub max_browser_renders: usize,
    pub requests_used: usize,
    pub browser_renders_used: usize,
}

impl TickBudgetLedger {
    pub fn new(budget: &SyncBudget) -> Self {
        let now = Instant::now();
        let wall_ms = budget.max_wall_clock_ms.max(1);
        let max_requests = budget
            .max_requests_per_tick
            .clamp(1, MAX_REQUESTS_PER_TICK_CAP);
        Self {
            deadline: checked_deadline(now, wall_ms),
            max_requests,
            max_browser_renders: budget.max_browser_renders_per_tick,
            requests_used: 0,
            browser_renders_used: 0,
        }
    }

    pub fn remaining_ms(&self) -> u64 {
        u64::try_from(
            self.deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub fn can_afford_browser(&self) -> bool {
        !self.is_expired()
            && self.requests_used < self.max_requests
            && self.browser_renders_used < self.max_browser_renders
    }

    pub fn browser_remaining_fraction(&self) -> f64 {
        if self.max_browser_renders == 0 {
            return 0.0;
        }
        (self
            .max_browser_renders
            .saturating_sub(self.browser_renders_used) as f64
            / self.max_browser_renders as f64)
            .clamp(0.0, 1.0)
    }

    /// 在实际执行任何网络请求（包含重写回退与浏览器升级）前原子占用配额。
    pub fn try_reserve(&mut self, is_browser: bool) -> bool {
        if self.is_expired() || self.requests_used >= self.max_requests {
            return false;
        }
        if is_browser && self.browser_renders_used >= self.max_browser_renders {
            return false;
        }
        self.requests_used = self.requests_used.saturating_add(1);
        if is_browser {
            self.browser_renders_used = self.browser_renders_used.saturating_add(1);
        }
        true
    }

    pub(super) async fn run_tick_bounded<F, T>(&self, fut: F) -> Result<T, TickStepError>
    where
        F: Future<Output = Result<T, ApiError>>,
    {
        if self.is_expired() {
            return Err(TickStepError::BudgetExhausted);
        }
        let res = tokio::time::timeout_at(self.deadline, fut)
            .await
            .map_err(|_| TickStepError::BudgetExhausted)?;
        res.map_err(TickStepError::Request)
    }

    /// 绑定当前 Tick 墙钟截止时间执行单次抓取；超时立即取消 in-flight Future。
    #[cfg(test)]
    pub async fn run_bounded<F, T>(&self, fut: F) -> Result<T, ApiError>
    where
        F: Future<Output = Result<T, ApiError>>,
    {
        self.run_tick_bounded(fut).await.map_err(|err| match err {
            TickStepError::BudgetExhausted => {
                ApiError::new(ErrorCode::Timeout, "sync tick wall-clock budget exhausted")
            }
            TickStepError::Request(e) => e,
        })
    }
}

pub(super) fn is_budget_exhausted_step<T>(
    ledger: &TickBudgetLedger,
    res: &Result<T, TickStepError>,
) -> bool {
    match res {
        Err(TickStepError::BudgetExhausted) => true,
        Err(TickStepError::Request(err))
            if err.code == ErrorCode::Timeout
                && (ledger.is_expired() || ledger.remaining_ms() <= 5) =>
        {
            true
        }
        _ => false,
    }
}

pub(super) fn unwrap_tick_step<T>(res: Result<T, TickStepError>) -> Result<T, ApiError> {
    match res {
        Ok(v) => Ok(v),
        Err(TickStepError::Request(e)) => Err(e),
        Err(TickStepError::BudgetExhausted) => Err(ApiError::new(
            ErrorCode::Timeout,
            "sync tick wall-clock budget exhausted",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn paused_clock_enforces_hard_request_browser_and_wall_budget_t07() {
        let spec = SyncBudget {
            max_requests_per_tick: 2,
            max_browser_renders_per_tick: 1,
            max_wall_clock_ms: 100,
            target_items: None,
        };
        let mut ledger = TickBudgetLedger::new(&spec);
        assert!(ledger.try_reserve(true));
        assert!(!ledger.can_afford_browser());
        assert!(!ledger.try_reserve(true));
        assert!(ledger.try_reserve(false));
        assert!(!ledger.try_reserve(false));
        assert_eq!(ledger.requests_used, 2);
        assert_eq!(ledger.browser_renders_used, 1);

        let err = ledger
            .run_bounded(async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok::<(), ApiError>(())
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(ledger.is_expired());
    }

    #[tokio::test]
    async fn huge_wall_clock_budget_never_overflows_instant_or_remaining_ms() {
        let spec = SyncBudget {
            max_requests_per_tick: 1,
            max_browser_renders_per_tick: 0,
            max_wall_clock_ms: u64::MAX,
            target_items: None,
        };
        let ledger = TickBudgetLedger::new(&spec);
        assert!(ledger.remaining_ms() > 0);
        let res = ledger
            .run_tick_bounded(async {
                Err::<(), ApiError>(ApiError::new(ErrorCode::DnsFailure, "nx"))
            })
            .await;
        assert!(matches!(
            res,
            Err(TickStepError::Request(e)) if e.code == ErrorCode::DnsFailure
        ));
    }
}
