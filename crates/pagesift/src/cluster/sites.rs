//! 工人的礼貌状态交接（[`SiteStates`]），接在 `pagesift-policy` 的退避表上。
//!
//! 站点换负责人时，旧负责人把该站点下各主机的剩余冷却时间（`Backoff::check`）交给新负责人，
//! 新负责人按协议给定的等待（同 Retry-After）记进自己的退避表（`Backoff::on_throttled`），
//! 网关在交接期内暂停该站点 `wait`（取各主机剩余冷却的最大值）。
//!
//! 只交接退避冷却：限速表的"下次可发时间"和每站点并发没有导出接口，不在交接包里；
//! 这部分由网关的默认暂停兜住（旧负责人的租约到期 + 一个往返上界）。
//!
//! 退避表不能按站点列举主机，所以这里记下本工人执行过的主机（按站点键分片）。
//! 没在冷却的主机没有要交接的状态，列举时顺手清掉；每个分片在条目数翻倍时清理一次
//! （动态表的摊还 O(1)，CLRS 3rd ed. §17.4），表的大小跟着在冷却的主机数走。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use pagesift_cluster::{SiteKey, SiteState, SiteStates};
use pagesift_stat::rng::splitmix64;

use crate::Scraper;

#[derive(Default)]
struct Hosts {
    by_site: HashMap<SiteKey, HashSet<String>>,
    entries: usize,
    prune_at: usize,
}

/// 独占缓存行（[推导] 128 字节：x86 相邻行预取成对取入两条 64 字节缓存行）。
#[repr(align(128))]
#[derive(Default)]
struct Shard(Mutex<Hosts>);

pub(crate) struct BackoffStates {
    scraper: Arc<Scraper>,
    shards: Box<[Shard]>,
    mask: u64,
}

impl BackoffStates {
    pub fn new(scraper: Arc<Scraper>) -> Self {
        let n = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .next_power_of_two();
        Self {
            scraper,
            shards: (0..n).map(|_| Shard::default()).collect(),
            mask: n as u64 - 1,
        }
    }

    fn shard(&self, site: SiteKey) -> MutexGuard<'_, Hosts> {
        self.shards[(splitmix64(site.0) & self.mask) as usize]
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn cooldown(&self, host: &str) -> Option<Duration> {
        self.scraper.policy().backoff().check(host).err()
    }

    /// 记下本工人执行过的主机。
    pub fn note(&self, url: &str) {
        let Some(host) = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        else {
            return;
        };
        let site = super::task::site_of(url);
        let mut s = self.shard(site);
        if s.by_site.entry(site).or_default().insert(host) {
            s.entries += 1;
        }
        if s.entries > s.prune_at {
            self.prune(&mut s);
            s.prune_at = s.entries * 2;
        }
    }

    fn prune(&self, s: &mut Hosts) {
        s.by_site.retain(|_, hosts| {
            hosts.retain(|h| self.cooldown(h).is_some());
            !hosts.is_empty()
        });
        s.entries = s.by_site.values().map(HashSet::len).sum();
    }
}

impl SiteStates for BackoffStates {
    fn sites(&self) -> Vec<SiteKey> {
        let mut out = Vec::new();
        for sh in self.shards.iter() {
            let mut s = sh.0.lock().unwrap_or_else(|e| e.into_inner());
            self.prune(&mut s);
            out.extend(s.by_site.keys().copied());
        }
        out
    }

    fn export(&self, site: SiteKey) -> Option<SiteState> {
        let hosts = self.shard(site).by_site.remove(&site)?;
        let waits: Vec<(String, u64)> = hosts
            .into_iter()
            .filter_map(|h| {
                let w = self.cooldown(&h)?;
                Some((h, u64::try_from(w.as_millis()).unwrap_or(u64::MAX)))
            })
            .collect();
        let wait = Duration::from_millis(waits.iter().map(|(_, w)| *w).max()?);
        let state = pagesift_sink::codec::to_vec(&waits).ok()?;
        Some(SiteState { site, wait, state })
    }

    fn import(&self, state: SiteState) {
        let Ok(waits) = pagesift_sink::codec::from_slice::<Vec<(String, u64)>>(&state.state) else {
            return;
        };
        let backoff = self.scraper.policy().backoff();
        let mut s = self.shard(state.site);
        for (host, ms) in waits {
            backoff.on_throttled(&host, Some(Duration::from_millis(ms)));
            if s.by_site.entry(state.site).or_default().insert(host) {
                s.entries += 1;
            }
        }
    }
}
