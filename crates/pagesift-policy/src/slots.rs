//! 名额数可在运行中调整的先进先出信号量。
//!
//! 包一层 tokio 的 [`Semaphore`]（公平、先进先出）：加名额直接 `add_permits`；减名额先收掉空闲的
//! （`forget_permits`），收不够的记成欠账，之后归还的名额先抵欠账、不再放回。这样减小上限不会打断
//! 正在进行的请求，排队顺序也不变。

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug)]
struct Inner {
    sem: Arc<Semaphore>,
    /// 目标名额数。
    limit: AtomicUsize,
    /// 已发出、归还时要收回的名额数。
    debt: AtomicUsize,
    /// 正在使用的名额数。
    active: AtomicUsize,
    resize: Mutex<()>,
}

/// 可调整上限的信号量。克隆共享同一组名额。
#[derive(Debug, Clone)]
pub struct Slots(Arc<Inner>);

/// 一个名额；丢弃时归还（有欠账时抵账）。
#[derive(Debug)]
pub struct Slot {
    permit: Option<OwnedSemaphorePermit>,
    inner: Arc<Inner>,
    /// 拿到时名额是否已经用满（需求受并发限制，控制器据此判断要不要往上探）。
    saturated: bool,
}

impl Slots {
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self(Arc::new(Inner {
            sem: Arc::new(Semaphore::new(limit)),
            limit: AtomicUsize::new(limit),
            debt: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            resize: Mutex::new(()),
        }))
    }

    pub fn limit(&self) -> usize {
        self.0.limit.load(Ordering::Acquire)
    }

    pub fn active(&self) -> usize {
        self.0.active.load(Ordering::Acquire)
    }

    /// 排队拿一个名额（先进先出）。
    pub async fn acquire(&self) -> Slot {
        let saturated = self.0.sem.available_permits() == 0;
        let permit = self
            .0
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        self.0.active.fetch_add(1, Ordering::AcqRel);
        Slot {
            permit: Some(permit),
            inner: self.0.clone(),
            saturated,
        }
    }

    /// 把上限改成 `limit`（至少 1）。
    pub fn set_limit(&self, limit: usize) {
        let limit = limit.max(1);
        let _guard = self.0.resize.lock().unwrap_or_else(|e| e.into_inner());
        let old = self.0.limit.swap(limit, Ordering::AcqRel);
        if limit > old {
            let mut add = limit - old;
            // 先抵欠账
            let mut debt = self.0.debt.load(Ordering::Acquire);
            while debt > 0 {
                let pay = debt.min(add);
                match self.0.debt.compare_exchange(
                    debt,
                    debt - pay,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        add -= pay;
                        break;
                    }
                    Err(now) => debt = now,
                }
            }
            if add > 0 {
                self.0.sem.add_permits(add);
            }
        } else if limit < old {
            let cut = old - limit;
            let forgot = self.0.sem.forget_permits(cut);
            if forgot < cut {
                self.0.debt.fetch_add(cut - forgot, Ordering::AcqRel);
            }
        }
    }
}

impl Slot {
    pub fn saturated(&self) -> bool {
        self.saturated
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::AcqRel);
        let Some(permit) = self.permit.take() else {
            return;
        };
        let mut debt = self.inner.debt.load(Ordering::Acquire);
        while debt > 0 {
            match self.inner.debt.compare_exchange(
                debt,
                debt - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    permit.forget();
                    return;
                }
                Err(now) => debt = now,
            }
        }
        drop(permit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn try_acquire(s: &Slots) -> Option<Slot> {
        tokio::time::timeout(Duration::from_millis(10), s.acquire())
            .await
            .ok()
    }

    #[tokio::test(start_paused = true)]
    async fn shrinking_takes_effect_as_permits_return_and_growing_pays_debt_first() {
        let s = Slots::new(3);
        let a = s.acquire().await;
        let b = s.acquire().await;
        let c = s.acquire().await;
        assert!(c.saturated() || !a.saturated());
        s.set_limit(1);
        drop(a);
        drop(b);
        assert!(try_acquire(&s).await.is_none(), "两个名额抵了欠账");
        drop(c);
        let d = try_acquire(&s).await.expect("limit 1");
        assert!(try_acquire(&s).await.is_none());
        s.set_limit(2);
        let e = try_acquire(&s).await.expect("grown to 2");
        assert_eq!((s.active(), s.limit()), (2, 2));
        // 有欠账时增加先抵账
        s.set_limit(1);
        s.set_limit(2);
        assert!(try_acquire(&s).await.is_none());
        drop((d, e));
        assert!(try_acquire(&s).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_are_served_in_order() {
        let s = Slots::new(1);
        let held = s.acquire().await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for i in 0..4 {
            let (s, tx) = (s.clone(), tx.clone());
            tokio::spawn(async move {
                let slot = s.acquire().await;
                tx.send(i).unwrap();
                drop(slot);
            });
            tokio::task::yield_now().await;
        }
        drop(held);
        let mut order = Vec::new();
        for _ in 0..4 {
            order.push(rx.recv().await.unwrap());
        }
        assert_eq!(order, [0, 1, 2, 3]);
    }
}
