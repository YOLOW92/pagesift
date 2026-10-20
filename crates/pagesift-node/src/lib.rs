//! npm 包 `pagesift`：Rust 核心外面的一层薄包装（napi-rs），按性能优先设计：
//!
//! - 批量接口（`convertMany`、`Scraper.scrapeMany`、`Scraper.scrapeStream`）的并发调度都在 Rust 侧，
//!   每批只跨一次 JS/Rust 边界（流式接口每页一次）；
//! - 结果是 [`page::Page`] 对象，大字段留在 Rust 侧，读到才复制成 JS 字符串；
//! - CPU 密集的转换在 Rust 的线程上跑，不占 Node 主线程，也不受 libuv 线程池大小限制。
//!
//! 这个 crate 不写 `#![forbid(unsafe_code)]`：napi-rs 的宏会生成 unsafe 代码（PRD 第 7 节规定绑定层是唯一的例外）。
//! 我们自己的代码里不写 unsafe。

mod convert;
mod error;
mod options;
mod page;
mod scraper;

pub use convert::{convert, convert_async, convert_many};
pub use error::BatchError;
pub use options::{BatchOptions, ConvertOptions, ScrapeOptions, ScraperConfig};
pub use page::Page;
pub use scraper::{BatchItem, ScrapeStream, Scraper};

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// 每个 Node 环境（主线程、各个 worker）各有一份。
struct EnvState {
    error_class: FunctionRef<FnArgs<(String, String)>, Unknown<'static>>,
}

#[napi(module_exports)]
pub fn init(mut exports: Object, env: Env) -> Result<()> {
    let class: Function<FnArgs<(String, String)>, Unknown<'static>> =
        env.run_script(error::CLASS_SOURCE)?;
    let error_class = class.create_ref()?;
    exports.set("PagesiftError", class)?;
    exports.set("version", env!("CARGO_PKG_VERSION"))?;
    env.set_instance_data(EnvState { error_class }, 0, |_| {})
}

fn state(env: &Env) -> Result<&'static mut EnvState> {
    env.get_instance_data::<EnvState>()?
        .ok_or_else(|| Error::from_reason("pagesift: module is not initialized"))
}
