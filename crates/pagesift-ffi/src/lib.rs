//! pagesift 的 C ABI（`libpagesift`）：JSON 进，JSON 出。函数说明和线程规则见 `include/pagesift.h`。
//!
//! 这个 crate 不写 `#![forbid(unsafe_code)]`：C 边界上的指针只能用 unsafe 读（绑定层是唯一的例外）。
//! unsafe 只出现在本文件，其余模块都是安全代码。Rust panic 不能穿过 C 边界，所以每个导出函数都
//! 包 `catch_unwind`，panic 变成 `INTERNAL_ERROR` 结果。异步都在内部的 tokio 运行时上阻塞执行。

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};

use pagesift_core::Scraper;
use pagesift_core::api::{ApiError, ErrorCode};
use serde_json::Value;
use tokio::runtime::Runtime;

mod args;
mod crawl;
mod doc;
mod ops;

use doc::failure_value;

/// 引擎句柄。C 这边只看得到不透明指针。
pub struct PagesiftEngine {
    scraper: Arc<Scraper>,
}

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("pagesift")
            .build()
            .expect("building the tokio runtime")
    })
}

/// `Value` → 交给调用方的 C 字符串。serde_json 把 NUL 转义成 `\u0000`，所以不会有内部 NUL。
fn into_c(v: &Value) -> *mut c_char {
    let text = serde_json::to_string(v).unwrap_or_else(|_| String::from("{\"success\":false}"));
    CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw)
}

fn internal(what: &str) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("internal error in {what}"))
}

/// 跑 `f`，panic 变成错误结果；返回值总是调用方要释放的 JSON 字符串。
fn guarded(what: &str, f: impl FnOnce() -> Result<Value, ApiError>) -> *mut c_char {
    let out = catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(internal(what)));
    into_c(&out.unwrap_or_else(|e| failure_value(&e)))
}

/// 读必填的字符串参数。
///
/// # Safety
/// `p` 为空，或指向以 NUL 结尾、读取期间有效的字符串。
unsafe fn required<'a>(p: *const c_char, name: &str) -> Result<&'a str, ApiError> {
    if p.is_null() {
        return Err(ApiError::invalid(format!("{name} must not be NULL")));
    }
    // SAFETY: 调用方保证 `p` 指向以 NUL 结尾的有效字符串
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| ApiError::invalid(format!("{name} must be valid UTF-8")))
}

/// 读可选的字符串参数（NULL 表示没给）。
///
/// # Safety
/// 同 [`required`]。
unsafe fn optional<'a>(p: *const c_char, name: &str) -> Result<Option<&'a str>, ApiError> {
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: 转述调用方的保证
    unsafe { required(p, name) }.map(Some)
}

/// 读引擎句柄。
///
/// # Safety
/// `engine` 为空，或是 `pagesift_engine_new` 返回、还没释放的句柄。
unsafe fn scraper<'a>(engine: *const PagesiftEngine) -> Result<&'a Arc<Scraper>, ApiError> {
    if engine.is_null() {
        return Err(ApiError::invalid("engine must not be NULL"));
    }
    // SAFETY: 调用方保证句柄有效；这里只读
    Ok(&unsafe { &*engine }.scraper)
}

/// 版本号，静态字符串，不要释放。
#[unsafe(no_mangle)]
pub extern "C" fn pagesift_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// 释放库返回的 JSON 字符串；NULL 无操作。
///
/// # Safety
/// `s` 为空，或是本库返回、还没释放的字符串。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: 调用方保证 `s` 来自 `CString::into_raw`，且只释放一次
    drop(unsafe { CString::from_raw(s) });
}

/// 建引擎。失败返回 NULL，原因写进 `*error_out`（给了的话）。
///
/// # Safety
/// `config_json` 为空或是有效的 C 字符串；`error_out` 为空或可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_engine_new(
    config_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut PagesiftEngine {
    if !error_out.is_null() {
        // SAFETY: 调用方保证 `error_out` 可写
        unsafe { *error_out = std::ptr::null_mut() };
    }
    let built = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 转述调用方的保证
        let cfg = unsafe { optional(config_json, "config_json") }
            .and_then(args::object)
            .and_then(args::engine_config)?;
        let _guard = runtime().enter();
        Scraper::new(cfg).map_err(|e| ApiError::invalid(e.to_string()))
    }))
    .unwrap_or_else(|_| Err(internal("pagesift_engine_new")));
    match built {
        Ok(s) => Box::into_raw(Box::new(PagesiftEngine {
            scraper: Arc::new(s),
        })),
        Err(e) => {
            if !error_out.is_null() {
                // SAFETY: 同上
                unsafe { *error_out = into_c(&failure_value(&e)) };
            }
            std::ptr::null_mut()
        }
    }
}

/// 关闭浏览器池并释放引擎；NULL 无操作。
///
/// # Safety
/// `engine` 为空，或是还没释放的句柄，且没有其他线程正在用它。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_engine_free(engine: *mut PagesiftEngine) {
    if engine.is_null() {
        return;
    }
    // SAFETY: 调用方保证句柄来自 `Box::into_raw`，且只释放一次
    let engine = unsafe { Box::from_raw(engine) };
    let _ = catch_unwind(AssertUnwindSafe(move || {
        let rt = runtime();
        rt.block_on(engine.scraper.shutdown());
        let _guard = rt.enter();
        drop(engine);
    }));
}

/// 抓一个页面，返回 Document JSON。
///
/// # Safety
/// 指针参数的要求见 `pagesift.h`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_scrape(
    engine: *const PagesiftEngine,
    url: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guarded("pagesift_scrape", || {
        // SAFETY: 转述调用方的保证
        let (s, url, o) = unsafe {
            (
                scraper(engine)?,
                required(url, "url")?,
                optional(options_json, "options_json")?,
            )
        };
        Ok(ops::scrape(runtime(), s, url.to_owned(), o))
    })
}

/// 解析内存里的文件，返回 Document JSON。
///
/// # Safety
/// `data` 在 `len > 0` 时指向 `len` 个可读字节；其余指针见 `pagesift.h`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_parse(
    engine: *const PagesiftEngine,
    data: *const u8,
    len: usize,
    filename: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guarded("pagesift_parse", || {
        if data.is_null() && len > 0 {
            return Err(ApiError::invalid("data must not be NULL when len > 0"));
        }
        // SAFETY: 转述调用方的保证；`data` 为空时 `len` 必为 0，走空切片
        let (s, bytes, name, o) = unsafe {
            (
                scraper(engine)?,
                if len == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(data, len)
                },
                required(filename, "filename")?,
                optional(options_json, "options_json")?,
            )
        };
        Ok(ops::parse(runtime(), s, bytes, name.to_owned(), o))
    })
}

/// 把 HTML 转成 Markdown，返回 Document JSON。不需要引擎。
///
/// # Safety
/// 指针参数的要求见 `pagesift.h`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_convert(
    html: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guarded("pagesift_convert", || {
        // SAFETY: 转述调用方的保证
        let (html, o) = unsafe {
            (
                required(html, "html")?,
                optional(options_json, "options_json")?,
            )
        };
        Ok(ops::convert(html, o))
    })
}

/// 批量抓取，全部完成后按输入顺序返回。`urls_json` 是字符串数组。
///
/// # Safety
/// 指针参数的要求见 `pagesift.h`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_batch(
    engine: *const PagesiftEngine,
    urls_json: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guarded("pagesift_batch", || {
        // SAFETY: 转述调用方的保证
        let (s, urls, o) = unsafe {
            (
                scraper(engine)?,
                required(urls_json, "urls_json")?,
                optional(options_json, "options_json")?,
            )
        };
        Ok(ops::batch(runtime(), s, urls, o))
    })
}

/// 站点链接列表。
///
/// # Safety
/// 指针参数的要求见 `pagesift.h`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_map(
    engine: *const PagesiftEngine,
    url: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guarded("pagesift_map", || {
        // SAFETY: 转述调用方的保证
        let (s, url, o) = unsafe {
            (
                scraper(engine)?,
                required(url, "url")?,
                optional(options_json, "options_json")?,
            )
        };
        Ok(ops::map(runtime(), s, url, o))
    })
}

/// 整站爬取，爬完一次返回。
///
/// # Safety
/// 指针参数的要求见 `pagesift.h`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pagesift_crawl(
    engine: *const PagesiftEngine,
    url: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guarded("pagesift_crawl", || {
        // SAFETY: 转述调用方的保证
        let (s, url, o) = unsafe {
            (
                scraper(engine)?,
                required(url, "url")?,
                optional(options_json, "options_json")?,
            )
        };
        Ok(crawl::crawl(runtime(), s, url, o))
    })
}
