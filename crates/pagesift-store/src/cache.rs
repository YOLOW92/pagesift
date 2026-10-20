//! 抓取结果缓存（F-8.2）。
//!
//! 参数语义以 Firecrawl v2 文档为准（2026-10 查阅）：
//! - <https://docs.firecrawl.dev/api-reference/endpoint/scrape>
//! - <https://docs.firecrawl.dev/features/fast-scraping>
//!
//! - `maxAge`（毫秒，默认 172800000 即 2 天）：缓存比这个年轻就直接返回，否则重新抓取；
//!   `maxAge: 0` 表示总是重新抓取。
//! - `minAge`（毫秒，无默认值）：设置后只查缓存、永不触发抓取；缓存至少要这么老才返回，
//!   否则返回 404 `SCRAPE_NO_CACHED_DATA`；设为 1 表示接受任何缓存。
//! - `storeInCache`（默认 true）：为 false 时结果不写入缓存。带 `headers`、`actions` 等参数的
//!   请求 Firecrawl 会强制不写缓存，这由调用方决定。
//!
//! 缓存键由调用方规范化：URL 加上影响结果的参数（Firecrawl 要求 url、mobile、location、
//! waitFor、blockAds、screenshot、代理模式完全一致才算命中）。

use std::time::{Duration, SystemTime};

use crate::engine::ns;
use crate::{Result, Store, from_ms, to_ms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub value: Vec<u8>,
    pub stored_at: SystemTime,
}

impl CacheEntry {
    /// 相对 `now` 的年龄；写入时间在未来（时钟回拨）时算 0。
    pub fn age_at(&self, now: SystemTime) -> Duration {
        now.duration_since(self.stored_at).unwrap_or_default()
    }
}

/// 一次请求的缓存参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    pub max_age: Duration,
    pub min_age: Option<Duration>,
    pub store_in_cache: bool,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            max_age: Duration::from_millis(172_800_000),
            min_age: None,
            store_in_cache: true,
        }
    }
}

impl CachePolicy {
    /// 直接用 Firecrawl 请求里的字段构造，缺省值和 Firecrawl 相同。
    pub fn from_firecrawl(
        max_age_ms: Option<u64>,
        min_age_ms: Option<u64>,
        store_in_cache: Option<bool>,
    ) -> Self {
        let d = Self::default();
        Self {
            max_age: max_age_ms.map_or(d.max_age, Duration::from_millis),
            min_age: min_age_ms.map(Duration::from_millis),
            store_in_cache: store_in_cache.unwrap_or(d.store_in_cache),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLookup {
    /// 命中，直接返回（响应里 `metadata.cacheState = "hit"`）。
    Hit(CacheEntry),
    /// 未命中，需要抓取；抓取后按 `store_in_cache` 决定是否写入。
    Miss,
    /// 设置了 `minAge` 但没有合格的缓存：返回 404 `SCRAPE_NO_CACHED_DATA`，不要抓取。
    NoCachedData,
}

pub struct Cache<'a> {
    store: &'a Store,
}

impl<'a> Cache<'a> {
    pub(crate) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// 读出缓存条目，不看年龄。
    pub fn entry(&self, key: impl AsRef<[u8]>) -> Result<Option<CacheEntry>> {
        Ok(self
            .store
            .get(ns::CACHE, key.as_ref())?
            .map(|(ts, value)| CacheEntry {
                value,
                stored_at: from_ms(ts),
            }))
    }

    /// `maxAge` 语义：年龄小于 `max_age` 才返回；`max_age` 为 0 时总是 None。
    pub fn get(&self, key: impl AsRef<[u8]>, max_age: Duration) -> Result<Option<CacheEntry>> {
        self.get_at(key, max_age, SystemTime::now())
    }

    pub fn get_at(
        &self,
        key: impl AsRef<[u8]>,
        max_age: Duration,
        now: SystemTime,
    ) -> Result<Option<CacheEntry>> {
        if max_age.is_zero() {
            return Ok(None);
        }
        Ok(self.entry(key)?.filter(|e| e.age_at(now) < max_age))
    }

    /// `minAge` 语义：年龄不小于 `min_age` 才返回。
    pub fn get_min_age(
        &self,
        key: impl AsRef<[u8]>,
        min_age: Duration,
        now: SystemTime,
    ) -> Result<Option<CacheEntry>> {
        Ok(self.entry(key)?.filter(|e| e.age_at(now) >= min_age))
    }

    /// 按一次请求的参数查缓存。设置了 `min_age` 时忽略 `max_age`（Firecrawl 文档里
    /// minAge 模式只查缓存，没有说明和 maxAge 的组合）。
    pub fn lookup(&self, key: impl AsRef<[u8]>, policy: &CachePolicy) -> Result<CacheLookup> {
        let now = SystemTime::now();
        if let Some(min) = policy.min_age {
            return Ok(match self.get_min_age(key, min, now)? {
                Some(e) => CacheLookup::Hit(e),
                None => CacheLookup::NoCachedData,
            });
        }
        Ok(match self.get_at(key, policy.max_age, now)? {
            Some(e) => CacheLookup::Hit(e),
            None => CacheLookup::Miss,
        })
    }

    pub fn put(&self, key: impl AsRef<[u8]>, value: &[u8]) -> Result<()> {
        self.put_at(key, value, SystemTime::now())
    }

    /// 指定写入时间（导入旧数据、测试）。
    pub fn put_at(&self, key: impl AsRef<[u8]>, value: &[u8], stored_at: SystemTime) -> Result<()> {
        self.store
            .put(ns::CACHE, key.as_ref(), value, to_ms(stored_at))
            .map(drop)
    }

    /// 按 `store_in_cache` 决定是否写入，返回是否写了。
    pub fn store(&self, key: impl AsRef<[u8]>, value: &[u8], policy: &CachePolicy) -> Result<bool> {
        if policy.store_in_cache {
            self.put(key, value)?;
        }
        Ok(policy.store_in_cache)
    }

    pub fn remove(&self, key: impl AsRef<[u8]>) -> Result<bool> {
        self.store.delete(ns::CACHE, key.as_ref())
    }
}
