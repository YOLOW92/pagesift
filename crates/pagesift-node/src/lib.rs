//! npm 包 `pagesift`：Rust 核心外面的一层薄包装（napi-rs）。
//!
//! 这个 crate 不写 `#![forbid(unsafe_code)]`：napi-rs 的宏会生成 unsafe 代码（PRD 第 7 节规定绑定层是唯一的例外）。
//! 我们自己的代码里不写 unsafe。抓取在 tokio 线程上跑，正文提取在阻塞线程池里，`convertAsync` 用 libuv 线程池，
//! 都不占 Node 的主线程。

mod convert;
mod error;
mod options;
mod scraper;

use napi::bindgen_prelude::*;
use napi_derive::napi;

pub use convert::{ConvertOptions, convert, convert_async};
pub use options::{ScrapeManyOptions, ScrapeOptions, ScraperConfig};
pub use scraper::{Scraper, scrape, scrape_many};

/// 每个 Node 环境（主线程、各个 worker）各有一份。
struct EnvState {
    error_class: FunctionRef<FnArgs<(String, String)>, Unknown<'static>>,
    default_scraper: Option<std::sync::Arc<pagesift::Scraper>>,
}

#[napi(module_exports)]
pub fn init(mut exports: Object, env: Env) -> Result<()> {
    let class: Function<FnArgs<(String, String)>, Unknown<'static>> =
        env.run_script(error::CLASS_SOURCE)?;
    let error_class = class.create_ref()?;
    exports.set("PagesiftError", class)?;
    exports.set("version", env!("CARGO_PKG_VERSION"))?;
    env.set_instance_data(
        EnvState {
            error_class,
            default_scraper: None,
        },
        0,
        |_| {},
    )
}

fn state(env: &Env) -> Result<&'static mut EnvState> {
    env.get_instance_data::<EnvState>()?
        .ok_or_else(|| Error::from_reason("pagesift: module is not initialized"))
}
