//! Access-barrier classification and bounded attempt orchestration.
//!
//! This crate does not perform network requests or solve challenges. The caller
//! must reserve each attempt before invoking an executor.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Served,
    Throttled,
    Login,
    Interstitial,
    Blocked,
    Challenge,
    Unreachable,
    Other,
}

/// Classify only evidence actually supplied by the caller. Status is considered
/// first for throttling, then a recognized barrier, then ordinary status codes.
pub fn classify(status: Option<u16>, retry_after: bool, barrier: Option<(&str, &str)>) -> Outcome {
    if status == Some(429) || (status == Some(503) && retry_after) {
        return Outcome::Throttled;
    }
    if let Some((kind, rule)) = barrier {
        if rule.contains("rate") {
            return Outcome::Throttled;
        }
        return match kind {
            "challenge" | "captcha" => Outcome::Challenge,
            "block" => Outcome::Blocked,
            "login_wall" => Outcome::Login,
            "interstitial" => Outcome::Interstitial,
            _ => Outcome::Other,
        };
    }
    match status {
        Some(401) => Outcome::Login,
        Some(403) => Outcome::Blocked,
        Some(200..=399) => Outcome::Served,
        _ => Outcome::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    Http,
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Finish,
    TryBrowser,
}

/// Shared network ceiling across HTTP redirect hops and browser requests.
#[derive(Debug)]
pub struct NetworkBudget {
    deadline: Instant,
    navigations: AtomicUsize,
    resources: AtomicUsize,
    browser_executions: AtomicUsize,
}

impl NetworkBudget {
    pub fn new(timeout: Duration, navigations: usize, resources: usize) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            navigations: AtomicUsize::new(navigations),
            resources: AtomicUsize::new(resources),
            browser_executions: AtomicUsize::new(0),
        }
    }

    pub fn reserve(&self, navigation: bool) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        let counter = if navigation {
            &self.navigations
        } else {
            &self.resources
        };
        let mut left = counter.load(Ordering::Acquire);
        loop {
            let Some(next) = left.checked_sub(1) else {
                return false;
            };
            match counter.compare_exchange_weak(left, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(actual) => left = actual,
            }
        }
    }

    pub fn browser_started(&self) {
        self.browser_executions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn browser_executions(&self) -> usize {
        self.browser_executions.load(Ordering::Relaxed)
    }
}

/// Per-request state. The deadline and limits are never reset between attempts.
pub struct Attempts {
    deadline: Instant,
    max_attempts: u32,
    max_browser_attempts: u32,
    attempts: u32,
    browsers: u32,
    in_flight: bool,
}

impl Attempts {
    pub fn new(timeout: Duration, max_attempts: u32, max_browser_attempts: u32) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            max_attempts,
            max_browser_attempts,
            attempts: 0,
            browsers: 0,
            in_flight: false,
        }
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn reserve(&mut self, executor: Executor) -> bool {
        if self.in_flight || self.remaining().is_zero() || self.attempts >= self.max_attempts {
            return false;
        }
        if executor == Executor::Browser && self.browsers >= self.max_browser_attempts {
            return false;
        }
        self.attempts += 1;
        self.browsers += u32::from(executor == Executor::Browser);
        self.in_flight = true;
        true
    }

    pub fn finish(
        &mut self,
        executor: Executor,
        outcome: Outcome,
        browser_allowed: bool,
    ) -> Decision {
        if !self.in_flight {
            return Decision::Finish;
        }
        self.in_flight = false;
        if executor == Executor::Http
            && browser_allowed
            && matches!(outcome, Outcome::Blocked | Outcome::Challenge)
            && self.attempts < self.max_attempts
            && self.browsers < self.max_browser_attempts
            && !self.remaining().is_zero()
        {
            Decision::TryBrowser
        } else {
            Decision::Finish
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_without_reservation_is_safe() {
        let mut attempts = Attempts::new(Duration::from_secs(10), 2, 1);
        assert_eq!(
            attempts.finish(Executor::Http, Outcome::Blocked, true),
            Decision::Finish
        );
        assert!(attempts.reserve(Executor::Http));
        assert_eq!(
            attempts.finish(Executor::Http, Outcome::Served, true),
            Decision::Finish
        );
        assert_eq!(
            attempts.finish(Executor::Http, Outcome::Blocked, true),
            Decision::Finish
        );
    }

    #[test]
    fn status_and_barriers_are_classified() {
        assert_eq!(
            classify(Some(429), false, Some(("captcha", "x"))),
            Outcome::Throttled
        );
        assert_eq!(classify(Some(503), true, None), Outcome::Throttled);
        assert_eq!(classify(Some(401), false, None), Outcome::Login);
        assert_eq!(
            classify(Some(200), false, Some(("captcha", "x"))),
            Outcome::Challenge
        );
        assert_eq!(
            classify(Some(200), false, Some(("block", "generic.rate_limited"))),
            Outcome::Throttled
        );
        assert_eq!(classify(Some(200), false, None), Outcome::Served);
    }

    #[test]
    fn bounded_state_machine() {
        let mut a = Attempts::new(Duration::from_secs(10), 2, 1);
        assert!(a.reserve(Executor::Http));
        assert!(!a.reserve(Executor::Browser));
        assert_eq!(
            a.finish(Executor::Http, Outcome::Challenge, true),
            Decision::TryBrowser
        );
        assert!(a.reserve(Executor::Browser));
        assert_eq!(
            a.finish(Executor::Browser, Outcome::Challenge, true),
            Decision::Finish
        );
        assert!(!a.reserve(Executor::Browser));
        let mut a = Attempts::new(Duration::ZERO, 2, 1);
        assert!(!a.reserve(Executor::Http));
    }

    #[test]
    fn throttling_and_login_never_escalate() {
        for outcome in [Outcome::Throttled, Outcome::Login, Outcome::Interstitial] {
            let mut a = Attempts::new(Duration::from_secs(1), 2, 1);
            assert!(a.reserve(Executor::Http));
            assert_eq!(a.finish(Executor::Http, outcome, true), Decision::Finish);
        }
    }

    #[test]
    fn network_budget_is_shared_and_bounded_under_concurrency() {
        let budget = std::sync::Arc::new(NetworkBudget::new(Duration::from_secs(1), 2, 3));
        assert!(budget.reserve(true));
        assert!(budget.reserve(true));
        assert!(!budget.reserve(true));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let b = budget.clone();
                std::thread::spawn(move || b.reserve(false))
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .filter(|ok| *ok)
                .count(),
            3
        );
        assert!(!NetworkBudget::new(Duration::ZERO, 1, 1).reserve(true));
    }
}
