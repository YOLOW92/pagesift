//! 会话池：把“cookie 罐 + 出口 + 请求档案”绑成一个 [`SessionSlot`]，按站点成池。
//!
//! - 被拦（出口池的结果分类里的 `Blocked`、`Challenge`）就把整个槽退役：cookie、出口、档案一起换掉，
//!   不只换出口（站点把会话标记了时，换出口留着旧 cookie 还是会被拦）；
//! - 限速（`Throttled`）不退役：退避由礼貌层记在站点上；`Neutral`、`Unreachable` 不怪会话；
//! - 池的大小由并发需求推出：同一站点同时在途的请求各用一个会话（共享 cookie 的并行请求会互相
//!   覆盖服务端的会话状态），所以池的大小等于这个站点的在途上限（Little 定律 L = λW 里的 L，
//!   由限速器的每站点并发给出）；需求下降时多余的槽在退役后不再补；
//! - 档案里 `fingerprint` 是可选的浏览器指纹（默认 `None`，见 `pagesift_net::FingerprintProfile`）；
//!   其余是请求层面的字段（`Accept-Language` 等）。池子还没接进抓取路径，这个字段目前只是类型上的绑定。

use std::collections::HashMap;
use std::sync::Mutex;

use crate::egress::Outcome;

/// 请求档案：会话内保持一致的请求头。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    pub accept_language: Option<String>,
    /// 这个会话用的浏览器指纹。
    pub fingerprint: Option<pagesift_net::FingerprintProfile>,
}

/// 一个会话：cookie、出口名（`None` 是默认出口）、档案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSlot {
    pub id: u64,
    pub egress: Option<String>,
    pub profile: Profile,
    /// `名字 → 值`（只保存服务端设置的 cookie）。
    pub cookies: Vec<(String, String)>,
    pub uses: u64,
}

impl SessionSlot {
    /// `Cookie` 请求头的值。
    pub fn cookie_header(&self) -> Option<String> {
        (!self.cookies.is_empty()).then(|| {
            self.cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

/// 报告结果后的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAction {
    Keep,
    /// 整个会话退役；重试时 [`SessionPool::acquire`] 会给一个新的。
    Retired,
}

#[derive(Default)]
struct Site {
    slots: Vec<SessionSlot>,
}

/// 按站点的会话池。
pub struct SessionPool {
    egresses: Vec<Option<String>>,
    profiles: Vec<Profile>,
    sites: Mutex<HashMap<String, Site>>,
    next: Mutex<u64>,
}

impl SessionPool {
    /// `egresses` 为空时只用默认出口；`profiles` 为空时用一个空档案。新槽依次轮换出口和档案。
    pub fn new(egresses: Vec<String>, profiles: Vec<Profile>) -> Self {
        let mut e: Vec<Option<String>> = egresses.into_iter().map(Some).collect();
        if e.is_empty() {
            e.push(None);
        }
        let p = if profiles.is_empty() {
            vec![Profile::default()]
        } else {
            profiles
        };
        Self {
            egresses: e,
            profiles: p,
            sites: Mutex::default(),
            next: Mutex::new(0),
        }
    }

    fn fresh(&self) -> SessionSlot {
        let mut n = self.next.lock().unwrap_or_else(|e| e.into_inner());
        let id = *n;
        *n += 1;
        SessionSlot {
            id,
            egress: self.egresses[id as usize % self.egresses.len()].clone(),
            profile: self.profiles[id as usize % self.profiles.len()].clone(),
            cookies: Vec::new(),
            uses: 0,
        }
    }

    /// 取一个会话：池子补到 `demand` 个（这个站点的在途上限），挑用得最少的。
    pub fn acquire(&self, site: &str, demand: usize) -> SessionSlot {
        let mut sites = self.sites.lock().unwrap_or_else(|e| e.into_inner());
        let s = sites.entry(site.to_string()).or_default();
        while s.slots.len() < demand.max(1) {
            s.slots.push(self.fresh());
        }
        let slot = s
            .slots
            .iter_mut()
            .min_by_key(|x| x.uses)
            .expect("at least one slot");
        slot.uses += 1;
        slot.clone()
    }

    /// 记下服务端给这个会话设置的 cookie（`Set-Cookie` 的 `名字=值` 部分）。
    pub fn set_cookies<'a>(
        &self,
        site: &str,
        id: u64,
        set_cookie: impl IntoIterator<Item = &'a str>,
    ) {
        let mut sites = self.sites.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = sites
            .get_mut(site)
            .and_then(|s| s.slots.iter_mut().find(|x| x.id == id))
        else {
            return;
        };
        for sc in set_cookie {
            let Some((k, v)) = sc.split(';').next().and_then(|kv| kv.split_once('=')) else {
                continue;
            };
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            match slot.cookies.iter_mut().find(|(n, _)| *n == k) {
                Some(c) => c.1 = v,
                None => slot.cookies.push((k, v)),
            }
        }
    }

    /// 报告结果：被拦就整个退役。
    pub fn report(&self, site: &str, id: u64, outcome: Outcome) -> SlotAction {
        if !matches!(outcome, Outcome::Blocked | Outcome::Challenge) {
            return SlotAction::Keep;
        }
        let mut sites = self.sites.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = sites.get_mut(site) {
            s.slots.retain(|x| x.id != id);
        }
        SlotAction::Retired
    }

    pub fn len(&self, site: &str) -> usize {
        self.sites
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(site)
            .map_or(0, |s| s.slots.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::rng::Rng;

    /// 模拟站点：没 cookie 时发 cookie 并返回 403；每个 cookie 用到一个几何分布的次数后被标记（之后 403）；
    /// 有时 429。返回 (状态码, 要设置的 cookie)。
    struct FakeSite {
        rng: Rng,
        next_cookie: u64,
        burn: HashMap<String, u64>,
    }

    impl FakeSite {
        fn get(&mut self, cookie: Option<&str>) -> (u16, Option<String>) {
            if self.rng.uniform() < 0.05 {
                return (429, None);
            }
            let Some(c) = cookie else {
                self.next_cookie += 1;
                return (403, Some(format!("sid=s{}", self.next_cookie)));
            };
            let left = self.burn.entry(c.to_string()).or_insert_with(|| {
                // 每个会话平均 20 次请求后被标记
                let mut k = 1;
                while self.rng.uniform() > 0.05 {
                    k += 1;
                }
                k
            });
            if *left == 0 {
                return (403, None);
            }
            *left -= 1;
            (200, None)
        }
    }

    /// 抓 `pages` 页，每页最多试 `tries` 次：返回 (完成数, 请求数)。
    fn run(pool: bool, pages: usize, tries: usize) -> (usize, usize) {
        let mut site = FakeSite {
            rng: Rng::seed(7),
            next_cookie: 0,
            burn: HashMap::new(),
        };
        let p = SessionPool::new(vec!["a".into(), "b".into()], vec![]);
        let (mut done, mut reqs) = (0, 0);
        let mut shared: Option<String> = None;
        for _ in 0..pages {
            for _ in 0..tries {
                reqs += 1;
                let slot = p.acquire("site", 2);
                let cookie = if pool {
                    slot.cookie_header()
                } else {
                    shared.clone()
                };
                let (status, set) = site.get(cookie.as_deref());
                if let Some(c) = &set {
                    if pool {
                        p.set_cookies("site", slot.id, [c.as_str()]);
                    } else {
                        shared = Some(c.clone());
                    }
                }
                if status == 200 {
                    done += 1;
                    break;
                }
                // 拿到新 cookie 的那次 403 不是被拦（只是还没有会话）
                if pool && set.is_none() {
                    p.report("site", slot.id, Outcome::from_status(status, false));
                }
            }
        }
        (done, reqs)
    }

    #[test]
    fn retiring_burned_sessions_completes_more_pages() {
        let (with, req_with) = run(true, 500, 3);
        let (without, req_without) = run(false, 500, 3);
        assert!(with > without, "pool {with} vs shared {without}");
        assert!(with >= 480, "{with}");
        // 每完成一页多花的请求
        let extra = |done: usize, reqs: usize| (reqs - done) as f64 / done.max(1) as f64;
        eprintln!(
            "session pool: completed {with}/500, extra requests per page {:.3}; shared session: {without}/500, {:.3}",
            extra(with, req_with),
            extra(without, req_without)
        );
        assert!(extra(with, req_with) < extra(without, req_without));
    }

    #[test]
    fn pool_size_follows_demand_and_retire_replaces() {
        let p = SessionPool::new(vec![], vec![]);
        let a = p.acquire("s", 3);
        assert_eq!(p.len("s"), 3);
        assert_eq!(p.report("s", a.id, Outcome::Throttled), SlotAction::Keep);
        assert_eq!(p.report("s", a.id, Outcome::Blocked), SlotAction::Retired);
        assert_eq!(p.len("s"), 2);
        let b = p.acquire("s", 3);
        assert_ne!(b.id, a.id);
        assert_eq!(p.len("s"), 3);
    }
}
