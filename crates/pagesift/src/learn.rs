//! 按站点自学习抓取方式：记住哪些站开浏览器没用、哪些站要跳过排在前面的内核。
//!
//! - `engine: auto` 时 HTTP 正文太少会升级到浏览器。同一来源、同一首级路径连续 [`STREAK`] 次
//!   渲染都没多出正文（本来就短的静态页），之后直接用 HTTP 结果。不能把静态文档的经验
//!   套到同站另一个尚未探测的应用路径；HTTP 短页面出现客户端代码时也不能直接跳过渲染。
//! - 同一个站连续 [`STREAK`] 次都要从某个内核回退（同源 iframe、脚本不支持），之后直接从回退到的
//!   内核开始。
//!
//! 每个站每 [`EXPLORE_EVERY`] 次照常走完整流程，站点改版后能重新学回来。表最多记
//! [`MAX_HOSTS`] 个站，满了清空重学：只是少一次优化，不影响结果。

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::Mutex;

pub const STREAK: u8 = 3;
pub const EXPLORE_EVERY: u32 = 16;
pub const MAX_HOSTS: usize = 100_000;
/// 渲染出的正文比 HTTP 多这么多字符才算浏览器有用。
pub const RENDER_GAIN_CHARS: usize = 200;

#[derive(Default, Clone, Copy)]
struct HostStats {
    /// 连续几次渲染没多出正文。
    render_useless: u8,
    /// 从第几个内核开始，以及连续几次从当前起点回退。
    engine_start: u8,
    engine_misses: u8,
    /// 这个站被问过几次，用来安排探索。
    asks: u32,
}

pub struct HostLearner {
    hasher: RandomState,
    hosts: Mutex<HashMap<u64, HostStats>>,
}

impl Default for HostLearner {
    fn default() -> Self {
        HostLearner {
            hasher: RandomState::new(),
            hosts: Mutex::new(HashMap::new()),
        }
    }
}

impl HostLearner {
    fn with<R>(&self, host: &str, f: impl FnOnce(&mut HostStats) -> R) -> R {
        let key = self.hasher.hash_one(host);
        let mut map = self.hosts.lock().unwrap();
        if map.len() >= MAX_HOSTS && !map.contains_key(&key) {
            map.clear();
        }
        f(map.entry(key).or_default())
    }

    /// 这次 HTTP 正文太少时，是否跳过浏览器直接用 HTTP 结果。
    pub fn skip_render(&self, url: &str) -> bool {
        self.with(&render_scope(url), |s| {
            s.asks = s.asks.wrapping_add(1);
            s.render_useless >= STREAK && s.asks % EXPLORE_EVERY != 0
        })
    }

    /// 记录一次升级渲染：渲染出的正文是否明显多于 HTTP。
    pub fn record_render(&self, url: &str, http_chars: usize, rendered_chars: usize) {
        let useful = rendered_chars >= http_chars + RENDER_GAIN_CHARS;
        self.with(&render_scope(url), |s| {
            s.render_useless = if useful {
                0
            } else {
                s.render_useless.saturating_add(1)
            };
        });
    }

    /// 这个站从第几个内核开始渲染。
    pub fn start_engine(&self, host: &str) -> usize {
        self.with(host, |s| {
            s.asks = s.asks.wrapping_add(1);
            if s.asks % EXPLORE_EVERY == 0 {
                0
            } else {
                s.engine_start as usize
            }
        })
    }

    /// 记录一次渲染：从 `from` 开始，最后用的是 `used`。
    pub fn record_engine(&self, host: &str, from: usize, used: usize) {
        let used = used.min(u8::MAX as usize) as u8;
        self.with(host, |s| {
            if from == 0 && used == 0 {
                // 探索时最前面的内核就够了：站点可能改版了，回到默认顺序
                *s = HostStats {
                    engine_start: 0,
                    engine_misses: 0,
                    ..*s
                };
            } else if used as usize > from {
                s.engine_misses = s.engine_misses.saturating_add(1);
                if s.engine_misses >= STREAK {
                    s.engine_start = used;
                    s.engine_misses = 0;
                }
            } else {
                s.engine_misses = 0;
            }
        });
    }
}

/// A conservative page-family boundary. Engine compatibility remains host-wide, but
/// evidence that rendering is useless must not suppress an unexplored section.
/// Query strings/fragments do not create families; origins (including ports) do.
fn render_scope(raw: &str) -> String {
    let Ok(url) = url::Url::parse(raw) else {
        return format!("render:{raw}");
    };
    let section = url
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    format!("render:{}/{}", url.origin().ascii_serialization(), section)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_sections_do_not_suppress_unexplored_dynamic_sections() {
        let l = HostLearner::default();
        for i in 0..STREAK {
            let url = format!("https://example.com/docs/{i}");
            assert!(!l.skip_render(&url));
            l.record_render(&url, 30, 30);
        }
        assert!(l.skip_render("https://example.com/docs/next?lang=ja#heading"));
        assert!(!l.skip_render("https://example.com/app/product"));
        assert!(!l.skip_render("https://example.com:8443/docs/next"));
        assert!(!l.skip_render("http://example.com/docs/next"));
        l.record_render("https://example.com/docs/next", 30, 500);
        assert!(!l.skip_render("https://example.com/docs/other"));
    }

    #[test]
    fn learns_to_skip_useless_renders_and_explores() {
        let l = HostLearner::default();
        for _ in 0..STREAK {
            assert!(!l.skip_render("a.test"));
            l.record_render("a.test", 300, 320);
        }
        let skipped = (0..EXPLORE_EVERY)
            .filter(|_| l.skip_render("a.test"))
            .count();
        assert_eq!(
            skipped,
            EXPLORE_EVERY as usize - 1,
            "one in EXPLORE_EVERY explores"
        );
        l.record_render("a.test", 300, 5000);
        assert!(!l.skip_render("a.test"));
        assert!(!l.skip_render("b.test"));
    }

    #[test]
    fn learns_the_starting_engine() {
        let l = HostLearner::default();
        for _ in 0..STREAK {
            let from = l.start_engine("spa.test");
            assert_eq!(from, 0);
            l.record_engine("spa.test", from, 1);
        }
        assert_eq!(l.start_engine("spa.test"), 1);
        l.record_engine("spa.test", 1, 1);
        // 探索时从 0 开始，0 就够用的话回到默认顺序
        let mut from = 1;
        while from != 0 {
            from = l.start_engine("spa.test");
        }
        l.record_engine("spa.test", 0, 0);
        assert_eq!(l.start_engine("spa.test"), 0);
    }
}
