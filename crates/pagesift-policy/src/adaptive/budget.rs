//! In-flight memory payload budgeter preventing unbounded concurrent allocation.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Protects the crawler from OOM by enforcing an upper bound on concurrent unparsed payload bytes.
#[derive(Clone)]
pub struct InflightBudget {
    semaphore: Arc<Semaphore>,
    budget_bytes: usize,
}

/// A permit representing an allocated portion of in-flight payload memory.
pub struct BudgetPermit {
    _permit: OwnedSemaphorePermit,
    bytes: usize,
}

impl BudgetPermit {
    /// Returns the number of bytes reserved by this permit.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl InflightBudget {
    /// Creates a new budget with the given byte capacity.
    pub fn new(budget_bytes: usize) -> Self {
        let permits = budget_bytes.clamp(1024, u32::MAX as usize) as u32;
        Self {
            semaphore: Arc::new(Semaphore::new(permits as usize)),
            budget_bytes,
        }
    }

    /// Returns the total configured budget in bytes.
    pub fn total_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Available bytes remaining in the budget.
    pub fn available_bytes(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Asynchronously acquires a permit for `bytes`.
    /// If requested bytes exceed available capacity, awaits until existing permits are released.
    pub async fn acquire(&self, bytes: usize) -> Result<BudgetPermit, ()> {
        let requested = (bytes as u32).min(self.budget_bytes as u32).max(1);
        match self.semaphore.clone().acquire_many_owned(requested).await {
            Ok(permit) => Ok(BudgetPermit {
                _permit: permit,
                bytes: requested as usize,
            }),
            Err(_) => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_inflight_budget() {
        let budget = InflightBudget::new(1024);
        assert_eq!(budget.total_bytes(), 1024);

        let p1 = budget.acquire(512).await.unwrap();
        assert_eq!(budget.available_bytes(), 512);

        let p2 = budget.acquire(256).await.unwrap();
        assert_eq!(budget.available_bytes(), 256);

        drop(p1);
        assert_eq!(budget.available_bytes(), 768);

        drop(p2);
        assert_eq!(budget.available_bytes(), 1024);
    }
}
