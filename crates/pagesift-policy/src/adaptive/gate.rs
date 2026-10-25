//! A cancellation-safe, dynamically resizable concurrency gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

#[derive(Debug)]
struct GateState {
    limit: AtomicUsize,
    active: AtomicUsize,
    notify: Notify,
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
                limit: AtomicUsize::new(limit.max(1)),
                active: AtomicUsize::new(0),
                notify: Notify::new(),
            }),
        }
    }

    pub fn limit(&self) -> usize {
        self.state.limit.load(Ordering::Acquire)
    }

    pub fn active(&self) -> usize {
        self.state.active.load(Ordering::Acquire)
    }

    pub fn set_limit(&self, limit: usize) {
        self.state.limit.store(limit.max(1), Ordering::Release);
        self.state.notify.notify_waiters();
    }

    pub async fn acquire(&self) -> AdaptiveConcurrencyPermit {
        loop {
            // Register before checking so a release cannot be lost between the
            // failed CAS and awaiting the notification.
            let notified = self.state.notify.notified();
            let active = self.state.active.load(Ordering::Acquire);
            let limit = self.state.limit.load(Ordering::Acquire);
            if active < limit
                && self
                    .state
                    .active
                    .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return AdaptiveConcurrencyPermit {
                    state: self.state.clone(),
                };
            }
            notified.await;
        }
    }

    /// Waits until all currently active permits have been released.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.state.notify.notified();
            if self.active() == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for AdaptiveConcurrencyPermit {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::AcqRel);
        self.state.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
