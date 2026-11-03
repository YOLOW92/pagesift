//! pagesift 的扩展点：抓取流程的四个阶段各有一种钩子。
//!
//! | 阶段 | trait | 能做什么 |
//! |---|---|---|
//! | 抓取前 | [`RequestHook`] | 改请求头、改网址、改选项，或跳过这一页 |
//! | 抓取后 | [`ResponseHook`] | 要求重试一次，或丢弃结果 |
//! | 抽取后 | [`DocumentHook`] | 改文档字段 |
//! | 输出时 | [`SinkHook`] | 改写或丢弃要写出的文档；由写结果的一方调用，内置入口里只有 CLI 的 crawl 经过它 |
//!
//! 每个钩子返回 [`Flow`]：`Continue`（照常往下）、`Replace`（换成新值，后面的钩子看到的是新值）、
//! `Pass`（这个钩子不管，交给下一个）。同一阶段的钩子按优先级（数字小的先）串成链，链走完回到
//! 默认实现。没有注册钩子时 [`Hooks::is_empty`] 为真，调用方整段跳过，每页只多一次判断。
//!
//! Rust 用户用 `Scraper::builder().hook(...)` 注册；服务端用户在 `pagesift.toml` 里写 `[[rules]]`，
//! 由 [`rules`] 编译成同样的钩子（不嵌脚本引擎、不做 WASM，不热加载）。

#![forbid(unsafe_code)]

pub mod rules;

use std::sync::Arc;

use pagesift_api::types::{Document, ScrapeOptions};

/// 钩子的决定。
#[derive(Debug, Clone, PartialEq)]
pub enum Flow<T> {
    /// 照常往下（后面的钩子继续看到当前值）。
    Continue,
    /// 换成这个值。
    Replace(T),
    /// 不管，交给下一个钩子。
    Pass,
}

/// 抓取前：一页的网址和选项。
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub url: String,
    pub options: ScrapeOptions,
}

/// 抓取前的钩子返回 `Replace(Request)` 改请求，或 [`RequestHook::skip`] 跳过。
pub trait RequestHook: Send + Sync {
    fn before(&self, req: &Request) -> Flow<Request>;
    /// 跳过这一页（返回原因）；默认不跳过。
    fn skip(&self, _req: &Request) -> Option<String> {
        None
    }
}

/// 抓取后的处置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 结果照用。
    Keep,
    /// 再抓一次（只重试一次）。
    Retry,
    /// 丢弃结果，报这个原因。
    Drop(String),
}

pub trait ResponseHook: Send + Sync {
    fn after(&self, url: &str, doc: &Document) -> Flow<Verdict>;
}

pub trait DocumentHook: Send + Sync {
    fn document(&self, url: &str, doc: &Document) -> Flow<Document>;
}

/// 输出时：返回 `Replace(None)` 不写这份文档。
pub trait SinkHook: Send + Sync {
    fn emit(&self, doc: &Document) -> Flow<Option<Document>>;
}

/// 一个钩子和它的优先级。
struct Slot<T: ?Sized> {
    priority: i32,
    hook: Arc<T>,
}

impl<T: ?Sized> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self {
            priority: self.priority,
            hook: self.hook.clone(),
        }
    }
}

/// 四条链。
#[derive(Clone, Default)]
pub struct Hooks {
    request: Vec<Slot<dyn RequestHook>>,
    response: Vec<Slot<dyn ResponseHook>>,
    document: Vec<Slot<dyn DocumentHook>>,
    sink: Vec<Slot<dyn SinkHook>>,
}

fn insert<T: ?Sized>(v: &mut Vec<Slot<T>>, priority: i32, hook: Arc<T>) {
    // 稳定插入：同优先级按注册顺序
    let at = v.partition_point(|s| s.priority <= priority);
    v.insert(at, Slot { priority, hook });
}

impl Hooks {
    pub fn is_empty(&self) -> bool {
        self.request.is_empty()
            && self.response.is_empty()
            && self.document.is_empty()
            && self.sink.is_empty()
    }

    pub fn request(&mut self, priority: i32, h: Arc<dyn RequestHook>) -> &mut Self {
        insert(&mut self.request, priority, h);
        self
    }
    pub fn response(&mut self, priority: i32, h: Arc<dyn ResponseHook>) -> &mut Self {
        insert(&mut self.response, priority, h);
        self
    }
    pub fn document(&mut self, priority: i32, h: Arc<dyn DocumentHook>) -> &mut Self {
        insert(&mut self.document, priority, h);
        self
    }
    pub fn sink(&mut self, priority: i32, h: Arc<dyn SinkHook>) -> &mut Self {
        insert(&mut self.sink, priority, h);
        self
    }

    /// 合并另一组钩子（规则编译出的和代码注册的放在一起）。
    pub fn extend(&mut self, other: &Hooks) {
        for s in &other.request {
            insert(&mut self.request, s.priority, s.hook.clone());
        }
        for s in &other.response {
            insert(&mut self.response, s.priority, s.hook.clone());
        }
        for s in &other.document {
            insert(&mut self.document, s.priority, s.hook.clone());
        }
        for s in &other.sink {
            insert(&mut self.sink, s.priority, s.hook.clone());
        }
    }

    /// 抓取前：`Err(原因)` 表示跳过。
    pub fn run_request(&self, mut req: Request) -> Result<Request, String> {
        for s in &self.request {
            if let Some(why) = s.hook.skip(&req) {
                return Err(why);
            }
            if let Flow::Replace(r) = s.hook.before(&req) {
                req = r;
            }
        }
        Ok(req)
    }

    /// 抓取后：第一个给出 `Replace` 的钩子决定；都不管时 `Keep`。
    pub fn run_response(&self, url: &str, doc: &Document) -> Verdict {
        for s in &self.response {
            if let Flow::Replace(v) = s.hook.after(url, doc) {
                return v;
            }
        }
        Verdict::Keep
    }

    pub fn run_document(&self, url: &str, mut doc: Document) -> Document {
        for s in &self.document {
            if let Flow::Replace(d) = s.hook.document(url, &doc) {
                doc = d;
            }
        }
        doc
    }

    /// 输出时：`None` 表示不写。
    pub fn run_sink(&self, doc: Document) -> Option<Document> {
        let mut cur = doc;
        for s in &self.sink {
            match s.hook.emit(&cur) {
                Flow::Replace(Some(d)) => cur = d,
                Flow::Replace(None) => return None,
                Flow::Continue | Flow::Pass => {}
            }
        }
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tag(&'static str);
    impl RequestHook for Tag {
        fn before(&self, req: &Request) -> Flow<Request> {
            let mut r = req.clone();
            r.options.headers.insert(
                "x-order".into(),
                format!(
                    "{}{}",
                    r.options
                        .headers
                        .get("x-order")
                        .cloned()
                        .unwrap_or_default(),
                    self.0
                ),
            );
            Flow::Replace(r)
        }
    }
    struct SkipAdmin;
    impl RequestHook for SkipAdmin {
        fn before(&self, _: &Request) -> Flow<Request> {
            Flow::Pass
        }
        fn skip(&self, req: &Request) -> Option<String> {
            req.url
                .contains("/admin")
                .then(|| "admin pages are skipped".into())
        }
    }

    #[test]
    fn chain_runs_in_priority_order_and_can_skip() {
        let mut h = Hooks::default();
        assert!(h.is_empty());
        h.request(10, Arc::new(Tag("b")))
            .request(0, Arc::new(Tag("a")))
            .request(5, Arc::new(SkipAdmin));
        let r = h
            .run_request(Request {
                url: "https://e.com/x".into(),
                options: Default::default(),
            })
            .unwrap();
        assert_eq!(r.options.headers["x-order"], "ab");
        assert!(
            h.run_request(Request {
                url: "https://e.com/admin".into(),
                options: Default::default()
            })
            .is_err()
        );
    }
}
