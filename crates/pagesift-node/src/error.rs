//! 把 pagesift 的 [`ApiError`] 变成 JS 里的 `PagesiftError`：`code` 和服务端的错误码一致。

use napi::bindgen_prelude::*;
use pagesift::api::ApiError;
use serde_json::{Value, json};

pub type Fallible<T> = std::result::Result<T, ApiError>;

/// 在模块加载时执行一次，得到 `PagesiftError` 类（napi 没法直接定义继承 `Error` 的类）。
pub const CLASS_SOURCE: &str = r#"(class PagesiftError extends Error {
  constructor(message, code) {
    super(message);
    this.name = 'PagesiftError';
    this.code = code;
  }
})"#;

pub fn to_js(env: &Env, e: &ApiError) -> Error {
    match build(env, e) {
        Ok(v) => v,
        Err(err) => err,
    }
}

fn build(env: &Env, e: &ApiError) -> Result<Error> {
    let class = crate::state(env)?.error_class.borrow_back(env)?;
    let obj = class.new_instance((e.message.clone(), e.code.as_str().to_string()).into())?;
    let mut obj = Object::from_unknown(obj)?;
    if let Some(d) = &e.details {
        obj.set("details", env.to_js_value(d)?)?;
    }
    if let Some(s) = e.retry_after {
        obj.set("retryAfter", s as f64)?;
    }
    Ok(Error::from(obj.to_unknown()))
}

/// `scrapeMany` 里单个 URL 失败时放进结果数组的对象，形状和 Firecrawl 的错误响应一致。
pub fn result(url: &str, e: &ApiError) -> Value {
    let mut v = json!({
        "success": false,
        "url": url,
        "code": e.code.as_str(),
        "error": e.message,
    });
    if let Some(d) = &e.details {
        v["details"] = d.clone();
    }
    if let Some(s) = e.retry_after {
        v["retryAfter"] = s.into();
    }
    v
}
