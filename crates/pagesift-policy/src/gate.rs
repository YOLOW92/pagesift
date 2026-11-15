//! 取消安全、上限可在运行中调整的并发闸门（爬取里同时进行的页面数用它）。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Notify;

const LIMIT_SHIFT: u32 = 32; // [推导] 64 位原子字高 32 位存并发上限、低 32 位存活跃许可数
const ACTIVE_MASK: u64 = 0xFFFF_FFFF; // [推导] 低 32 位活跃许可数掩码 (2^32 - 1)

#[inline]
fn pack_state(limit: usize, active: usize) -> u64 {
    let l = limit.clamp(1, u32::MAX as usize) as u64;
    let a = active.min(u32::MAX as usize) as u64;
    (l << LIMIT_SHIFT) | a
}

#[inline]
fn unpack_state(packed: u64) -> (usize, usize) {
    let limit = (packed >> LIMIT_SHIFT) as usize;
    let active = (packed & ACTIVE_MASK) as usize;
    (limit, active)
}

#[derive(Debug)]
struct GateState {
    packed: AtomicU64,
    notify_acquire: Notify,
    notify_idle: Notify,
}

/// Limits active work while allowing the target to change without recreating a semaphore.
#[derive(Debug, Clone)]
pub struct AdaptiveConcurrencyGate {
    state: Arc<GateState>,
}

/// RAII permit. Dropping or cancelling a task always releases its active slot.
#[derive(Debug)]
pub struct AdaptiveConcurrencyPermit {
    state: Arc<GateState>,
}

impl AdaptiveConcurrencyGate {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Arc::new(GateState {
                packed: AtomicU64::new(pack_state(limit, 0)),
                notify_acquire: Notify::new(),
                notify_idle: Notify::new(),
            }),
        }
    }

    pub fn limit(&self) -> usize {
        unpack_state(self.state.packed.load(Ordering::Acquire)).0
    }

    pub fn active(&self) -> usize {
        unpack_state(self.state.packed.load(Ordering::Acquire)).1
    }

    pub fn set_limit(&self, limit: usize) {
        let target = limit.clamp(1, u32::MAX as usize) as u64;
        loop {
            let cur = self.state.packed.load(Ordering::SeqCst);
            let active = cur & ACTIVE_MASK;
            let next = (target << LIMIT_SHIFT) | active;
            if self
                .state
                .packed
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        self.state.notify_acquire.notify_waiters();
    }

    pub async fn acquire(&self) -> AdaptiveConcurrencyPermit {
        loop {
            // 先调用 enable 把等待者挂进通知队列，再检查容量，避免 set_limit/release 与休眠之间丢唤醒。
            let notified = self.state.notify_acquire.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            loop {
                let cur = self.state.packed.load(Ordering::SeqCst);
                let (limit, active) = unpack_state(cur);
                if active >= limit {
                    break;
                }
                let next = pack_state(limit, active + 1);
                if self
                    .state
                    .packed
                    .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::Acquire)
                    .is_ok()
                {
                    return AdaptiveConcurrencyPermit {
                        state: self.state.clone(),
                    };
                }
            }
            notified.await;
        }
    }

    /// Waits until all currently active permits have been released.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.state.notify_idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if unpack_state(self.state.packed.load(Ordering::SeqCst)).1 == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for AdaptiveConcurrencyPermit {
    fn drop(&mut self) {
        let prev = self.state.packed.fetch_sub(1, Ordering::SeqCst);
        if unpack_state(prev).1 == 1 {
            self.state.notify_idle.notify_waiters();
        }
        self.state.notify_acquire.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn lowering_limit_blocks_new_work_until_active_drops_below_target() {
        let gate = AdaptiveConcurrencyGate::new(3);
        let p1 = gate.acquire().await;
        let p2 = gate.acquire().await;
        let p3 = gate.acquire().await;
        gate.set_limit(1);

        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.acquire().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(p1);
        drop(p2);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(p3);
        assert!(waiter.await.is_ok());
    }

    #[tokio::test]
    async fn raising_limit_wakes_waiters() {
        let gate = AdaptiveConcurrencyGate::new(1);
        let permit = gate.acquire().await;
        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.acquire().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        gate.set_limit(2);
        assert!(waiter.await.is_ok());
        drop(permit);
    }

    #[tokio::test]
    async fn wait_idle_tracks_permit_drop() {
        let gate = AdaptiveConcurrencyGate::new(2);
        let permit = gate.acquire().await;
        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.wait_idle().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(permit);
        assert!(waiter.await.is_ok());
    }

    #[tokio::test]
    async fn multiple_wait_idle_waiters_and_acquire_coexist_without_starvation() {
        let gate = AdaptiveConcurrencyGate::new(1);
        let p1 = gate.acquire().await;

        let idle1 = tokio::spawn({
            let g = gate.clone();
            async move { g.wait_idle().await }
        });
        let idle2 = tokio::spawn({
            let g = gate.clone();
            async move { g.wait_idle().await }
        });
        let idle3 = tokio::spawn({
            let g = gate.clone();
            async move { g.wait_idle().await }
        });
        let acq = tokio::spawn({
            let g = gate.clone();
            async move {
                let p = g.acquire().await;
                drop(p);
            }
        });
        tokio::task::yield_now().await;
        drop(p1);

        tokio::time::timeout(Duration::from_secs(2), async {
            idle1.await.unwrap();
            idle2.await.unwrap();
            idle3.await.unwrap();
            acq.await.unwrap();
        })
        .await
        .expect("all wait_idle and acquire waiters must wake cleanly");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn high_contention_cas_and_dynamic_limits_never_stall() {
        let gate = AdaptiveConcurrencyGate::new(8);
        let mut set = tokio::task::JoinSet::new();
        for i in 0..200usize {
            let g = gate.clone();
            set.spawn(async move {
                let permit = g.acquire().await;
                if i % 17 == 0 {
                    g.set_limit(if i % 34 == 0 { 2 } else { 12 });
                }
                tokio::task::yield_now().await;
                drop(permit);
            });
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(res) = set.join_next().await {
                res.unwrap();
            }
            gate.wait_idle().await;
        })
        .await
        .expect("contended gate must complete and become idle");
        assert_eq!(gate.active(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_limit_reduction_admission() {
        let gate = AdaptiveConcurrencyGate::new(64);
        let held = gate.acquire().await;
        let barrier = Arc::new(tokio::sync::Barrier::new(5));
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let g = gate.clone();
            let b = barrier.clone();
            set.spawn(async move {
                b.wait().await;
                for _ in 0..200 {
                    g.set_limit(1);
                    let (lim, act) = unpack_state(g.state.packed.load(Ordering::SeqCst));
                    assert!(lim != 1 || act <= 64, "packed state must remain consistent");
                    g.set_limit(2);
                    let p = g.acquire().await;
                    let (lim_after, act_after) =
                        unpack_state(g.state.packed.load(Ordering::SeqCst));
                    assert!(
                        act_after <= lim_after.max(2),
                        "active {act_after} exceeded concurrent limit {lim_after}"
                    );
                    drop(p);
                }
            });
        }
        barrier.wait().await;
        while let Some(res) = set.join_next().await {
            res.unwrap();
        }
        gate.set_limit(1);
        let waiter = tokio::spawn({
            let g = gate.clone();
            async move { g.acquire().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "must not admit when limit == 1 and active == 1"
        );
        drop(held);
        let p2 = waiter.await.unwrap();
        drop(p2);
        assert_eq!(gate.active(), 0);
    }
}
