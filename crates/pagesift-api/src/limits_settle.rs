//! 页面配额预留租约（[`PageHold`]）、三态持久化结算结果（[`SettleOutcome`]）及引擎侧 [`QuotaBudget`] 封装。

use std::sync::Arc;

use super::{Caller, Limiter, ledger_error};
use crate::error::ApiError;

/// 配额结算的持久化结果：明确区分已确认落盘、明确失败（未落盘）与结果模糊（I/O 错误等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettleOutcome {
    /// 内存与持久存储/账本均已确认落盘。
    Confirmed,
    /// 明确未持久化（如账本租约过期或已降级为 follower）；`recoverable` 指示是否已排队待核销。
    Failed { message: String, recoverable: bool },
    /// 持久化结果模糊（如底层 WAL/Store I/O 报错，无法确认是否已落盘），已登记待按 ID 幂等核销。
    Uncertain { message: String, recoverable: bool },
}

impl SettleOutcome {
    pub(crate) fn is_unrecoverable(&self) -> bool {
        matches!(
            self,
            Self::Failed {
                recoverable: false,
                ..
            } | Self::Uncertain {
                recoverable: false,
                ..
            }
        )
    }

    pub(crate) fn message(&self) -> Option<&str> {
        match self {
            Self::Confirmed => None,
            Self::Failed { message, .. } | Self::Uncertain { message, .. } => Some(message),
        }
    }

    pub(crate) fn warning(&self) -> Option<String> {
        match self {
            Self::Confirmed => None,
            Self::Failed { message, .. } => {
                Some(format!("quota_settlement_unconfirmed: failed: {message}"))
            }
            Self::Uncertain { message, .. } => Some(format!(
                "quota_settlement_unconfirmed: uncertain: {message}"
            )),
        }
    }
}

/// 账本短暂不可写时挂起的一笔冻结核销（同一 `hold.id` 终局意图唯一且在账本侧严格幂等）。
#[derive(Clone)]
pub(super) struct PendingLedgerClose {
    pub(super) hold: pagesift_cluster::Hold,
    /// `Some(actual)` 表示终局结算；`None` 表示终局释放。一旦确定绝不翻转。
    pub(super) settle: Option<u64>,
}

/// 一页的预留。`commit` 结算并返回持久化结果；未结算就丢弃时自动退还（失败、取消、panic 都走同一条路）。
pub(crate) struct PageHold {
    pub(super) limiter: Arc<Limiter>,
    pub(super) account: Arc<str>,
    pub(super) day: u64,
    pub(super) amount: u64,
    pub(super) ledger: Option<pagesift_cluster::Hold>,
    pub(super) tracked: bool,
    pub(super) settled: bool,
}

impl PageHold {
    pub(crate) async fn commit(mut self) -> SettleOutcome {
        if !self.tracked || self.settled {
            self.settled = true;
            return SettleOutcome::Confirmed;
        }
        if let Some(ledger) = &self.limiter.ledger {
            // 先标记已进入终局结算，杜绝 Drop 误发相反意图（Release）。
            self.settled = true;
            let Some(hold) = self.ledger.take() else {
                return SettleOutcome::Confirmed;
            };
            return match ledger.settle(&hold, self.amount) {
                Ok(()) => {
                    self.limiter.reconcile_ledger();
                    SettleOutcome::Confirmed
                }
                Err(e) => {
                    let uncertain = !matches!(e, pagesift_cluster::Error::Rejected(_));
                    self.limiter.note_failure(uncertain);
                    let recoverable = self
                        .limiter
                        .enqueue_ledger_close(hold.clone(), Some(self.amount));
                    tracing::error!(
                        account = %hold.account,
                        hold_id = hold.id,
                        uncertain,
                        recoverable,
                        error = %e,
                        "settling page quota in cluster ledger failed"
                    );
                    if uncertain {
                        SettleOutcome::Uncertain {
                            message: e.to_string(),
                            recoverable,
                        }
                    } else {
                        SettleOutcome::Failed {
                            message: e.to_string(),
                            recoverable,
                        }
                    }
                }
            };
        }
        let changed = self
            .limiter
            .settle_in_memory(&self.account, self.day, self.amount);
        self.settled = true;
        if changed {
            self.limiter.persist_usage(&self.account).await
        } else {
            SettleOutcome::Confirmed
        }
    }
}

impl Drop for PageHold {
    fn drop(&mut self) {
        if self.settled || !self.tracked {
            return;
        }
        if let Some(ledger) = &self.limiter.ledger {
            if let Some(hold) = self.ledger.take()
                && let Err(e) = ledger.release(&hold)
            {
                let uncertain = !matches!(e, pagesift_cluster::Error::Rejected(_));
                self.limiter.note_failure(uncertain);
                let recoverable = self.limiter.enqueue_ledger_close(hold.clone(), None);
                tracing::error!(
                    account = %hold.account,
                    hold_id = hold.id,
                    uncertain,
                    recoverable,
                    error = %e,
                    "releasing page quota in cluster ledger failed"
                );
            }
            return;
        }
        self.limiter.refund(&self.account, self.day, self.amount);
    }
}

/// 将 HTTP 层的 [`Limiter`] 与 [`Caller`] 封装成引擎可消费的 [`crate::PageBudget`]：
/// 每次请求前原子预留 [`PageHold`]，成功时通过 [`crate::PageLease::commit`] 结算，失败或取消时自动退还。
pub(crate) struct QuotaBudget {
    limiter: Arc<Limiter>,
    caller: Caller,
}

impl QuotaBudget {
    pub(crate) fn new(limiter: Arc<Limiter>, caller: Caller) -> Self {
        Self { limiter, caller }
    }
}

struct PageHoldLease(Option<PageHold>);

impl crate::PageLease for PageHoldLease {
    fn commit(mut self: Box<Self>) -> crate::BoxFuture<'static, ()> {
        let hold = self.0.take();
        Box::pin(async move {
            if let Some(h) = hold {
                let _ = h.commit().await;
            }
        })
    }
}

impl crate::PageBudget for QuotaBudget {
    fn acquire(&self) -> crate::BoxFuture<'_, Result<(), ApiError>> {
        Box::pin(async move {
            let hold = self.limiter.reserve_page(&self.caller).await?;
            let outcome = hold.commit().await;
            if outcome.is_unrecoverable() {
                return Err(ledger_error(
                    outcome
                        .message()
                        .unwrap_or("unrecoverable quota settlement failure"),
                ));
            }
            Ok(())
        })
    }

    fn reserve(&self) -> crate::BoxFuture<'_, Result<Box<dyn crate::PageLease>, ApiError>> {
        Box::pin(async move {
            let hold = self.limiter.reserve_page(&self.caller).await?;
            Ok(Box::new(PageHoldLease(Some(hold))) as Box<dyn crate::PageLease>)
        })
    }

    fn remaining(&self) -> Option<u64> {
        self.limiter.remaining_pages(&self.caller)
    }
}
