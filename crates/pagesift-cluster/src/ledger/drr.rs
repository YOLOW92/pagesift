//! 账户之间的加权亏空轮转（Deficit Round Robin）。
//!
//! M. Shreedhar, G. Varghese, "Efficient Fair Queuing Using Deficit Round Robin",
//! SIGCOMM 1995（IEEE/ACM ToN 4(3), 1996）：活动队列轮转，轮到队列 `i` 时亏空计数 `DCᵢ += Qᵢ`，
//! 只要 `DCᵢ` 够付队头分组的大小就发送并扣减；队列空了 `DCᵢ` 清零。每个队列长期得到的服务
//! 正比于 `Qᵢ`，与其他队列是否积压无关；空闲的队列不积累额度，积压的队列用满空出来的份额。
//!
//! 这里一个"分组"是一次分配（大小 1），`Qᵢ = wᵢ / min w`，权重是账户的每日配额（没设配额的
//! 账户和不带账户的任务取已知配额的最大值，都没设时为 1）。原文要求 `Qᵢ ≥` 最大分组，
//! 取 `min w` 作分母正好满足，所以每次轮到都至少能发一个，`O(1)`。
//! 与原文的差别：调用方一次只要一个分配，所以每次只从当前队列发一个，队列"够付但还没发完"
//! 时保持在队头等下一次拉取。
//!
//! 每个拉取线程各跑一份轮转（线程局部的亏空计数），热路径上不写任何共享量：每个线程各自
//! 按 `Qᵢ` 的比例发放，合起来的比例也是 `Qᵢ`。账户与权重登记在全局表里，只在新账户出现、
//! 权重改变时写一次并把代数加一；线程看到代数变了才去全局表同步。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::Ledger;
use crate::types::{Error, Lease, Owner, Resources, WorkerId, lock};

/// 调度单位：账户（`None` 是不带账户的任务）。
pub(crate) type Acct = Option<Arc<str>>;

/// 全局登记：出现过的账户与权重。
#[derive(Debug, Default)]
pub(crate) struct Registry {
    accounts: Vec<Acct>,
    weight: HashMap<Arc<str>, f64>,
}

impl Registry {
    pub(crate) fn len(&self) -> usize {
        self.accounts.len()
    }

    /// 登记一个账户，返回是否是新的。第一次出现账户时把不带账户的任务也登记进来，两者一起轮转。
    pub(crate) fn add(&mut self, a: &Acct) -> bool {
        if self.accounts.contains(a) {
            return false;
        }
        self.accounts.push(a.clone());
        if a.is_some() {
            self.add(&None);
        }
        true
    }

    pub(crate) fn set_weight(&mut self, a: &Arc<str>, w: Option<u64>) {
        match w {
            Some(w) if w > 0 => self.weight.insert(a.clone(), w as f64),
            _ => self.weight.remove(a),
        };
    }
}

/// 一个线程上的轮转状态。
#[derive(Debug, Default)]
pub(crate) struct Drr {
    ring: VecDeque<Acct>,
    deficit: HashMap<Acct, f64>,
    weight: HashMap<Arc<str>, f64>,
    /// 队头这一轮是否已经加过额度。
    charged: bool,
    /// 同步到的登记表代数。
    pub(crate) generation: u64,
}

impl Drr {
    pub(crate) fn len(&self) -> usize {
        self.ring.len()
    }

    pub(crate) fn knows(&self, a: &Acct) -> bool {
        self.deficit.contains_key(a)
    }

    /// 从全局登记表同步：新账户排到队尾，已有账户的亏空保留。
    pub(crate) fn sync(&mut self, reg: &Registry, generation: u64) {
        for a in &reg.accounts {
            if !self.deficit.contains_key(a) {
                self.deficit.insert(a.clone(), 0.0);
                self.ring.push_back(a.clone());
            }
        }
        self.weight.clone_from(&reg.weight);
        self.generation = generation;
    }

    fn quantum(&self, a: &Acct) -> f64 {
        let min = self.weight.values().copied().fold(f64::INFINITY, f64::min);
        if !min.is_finite() {
            return 1.0;
        }
        let max = self.weight.values().copied().fold(0.0, f64::max);
        let w = a.as_ref().and_then(|a| self.weight.get(a)).copied();
        w.unwrap_or(max) / min
    }

    fn rotate(&mut self) {
        self.ring.rotate_left(1);
        self.charged = false;
    }

    /// 当前该服务的账户。
    pub(crate) fn front(&mut self) -> Option<Acct> {
        let a = self.ring.front()?.clone();
        if !self.charged {
            let q = self.quantum(&a);
            *self.deficit.entry(a.clone()).or_default() += q;
            self.charged = true;
        }
        if self.deficit[&a] < 1.0 {
            self.rotate();
            return self.front();
        }
        Some(a)
    }

    /// 给 `a` 发出了一个分配。额度用完时轮到下一个。
    pub(crate) fn served(&mut self, a: &Acct) {
        let d = self.deficit.entry(a.clone()).or_default();
        *d -= 1.0;
        if *d < 1.0 {
            self.rotate();
        }
    }

    /// `a` 现在没有可做的任务（或配额用完）：亏空清零，轮到下一个。
    pub(crate) fn idle(&mut self, a: &Acct) {
        self.deficit.insert(a.clone(), 0.0);
        self.rotate();
    }
}

thread_local! {
    /// 本线程在各账本上的轮转状态（按账本编号）。
    static LOCAL: RefCell<HashMap<u64, Drr>> = RefCell::new(HashMap::new());
}

impl Ledger {
    /// 在本线程的轮转状态上做事；登记表变过时先同步（只有这时拿全局锁）。
    fn with_drr<R>(&self, f: impl FnOnce(&mut Drr) -> R) -> R {
        let generation = self.generation.load(Ordering::Acquire);
        LOCAL.with(|m| {
            let mut m = m.borrow_mut();
            let d = m.entry(self.id).or_default();
            if d.generation != generation {
                d.sync(&lock(&self.registry), generation);
            }
            f(d)
        })
    }

    pub(super) fn add_account(&self, a: &Acct) {
        if a.is_none() || self.with_drr(|d| d.knows(a)) {
            return;
        }
        let mut reg = lock(&self.registry);
        if reg.add(a) {
            self.accounts.store(reg.len(), Ordering::Release);
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// 多账户：本线程的加权亏空轮转选账户，再在这个账户的任务里拣。
    pub(super) fn pull_fair(
        &self,
        term: u64,
        worker: Option<&WorkerId>,
        owner: &Owner,
        free: &Resources,
        rtt_hi: Duration,
    ) -> Result<Option<Lease>, Error> {
        self.with_drr(|d| {
            for _ in 0..d.len() {
                let Some(a) = d.front() else { break };
                match self.pull_for(term, worker, owner, &a, free, rtt_hi)? {
                    Some(l) => {
                        d.served(&a);
                        return Ok(Some(l));
                    }
                    None => d.idle(&a),
                }
            }
            Ok(None)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两个账户 1:3 积压时发放比例正好 1:3；一个空闲时另一个用满。
    #[test]
    fn weighted_shares_and_work_conservation() {
        let (a, b): (Acct, Acct) = (Some("a".into()), Some("b".into()));
        let mut reg = Registry::default();
        reg.set_weight(a.as_ref().unwrap(), Some(100));
        reg.set_weight(b.as_ref().unwrap(), Some(300));
        assert!(reg.add(&a) && reg.add(&b) && !reg.add(&a));
        assert_eq!(reg.len(), 3, "accounts plus tasks without an account");
        let mut d = Drr::default();
        d.sync(&reg, 1);
        let mut n: HashMap<Acct, u32> = HashMap::new();
        for _ in 0..4000 {
            let x = d.front().unwrap();
            if x.is_none() {
                d.idle(&x);
                continue;
            }
            *n.entry(x.clone()).or_default() += 1;
            d.served(&x);
        }
        // 一轮里 a 发 1 个、b 发 3 个；截在轮中间时最多差一轮
        assert!(
            (i64::from(n[&a]) * 3 - i64::from(n[&b])).abs() <= 3,
            "{n:?}"
        );
        // a 空闲：每次分配都给 b，不会因为轮到 a 而空转
        for _ in 0..100 {
            loop {
                let x = d.front().unwrap();
                if x == b {
                    d.served(&x);
                    break;
                }
                d.idle(&x);
            }
        }
    }
}
