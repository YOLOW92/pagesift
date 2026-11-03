//! 错误：单个调用抛出 `PagesiftError`；批量结果里放 [`BatchError`] 普通对象。`code` 都和服务端的错误码一致。

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::ApiError;
use serde_json::Value;

pub type Fallible<T> = std::result::Result<T, ApiError>;

/// pagesift 自己的入口不接收未知字段：JS 传来的选项对象先作为 JSON 收下，未知字段全部列出后拒收，
/// 再按类型解析（napi 的对象转换会悄悄丢掉不认识的键）。没传或传了 `null`/`undefined` 时用默认值。
pub fn strict<T>(v: Option<Value>) -> Fallible<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(T::default());
    };
    if let Some(obj) = v.as_object() {
        pagesift::api::fields::reject_unknown(obj, &[pagesift::api::fields::names::<T>()])?;
    }
    serde_json::from_value(v).map_err(|e| ApiError::invalid(format!("invalid options: {e}")))
}

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

/// 批量结果里单个 URL 的错误：字段和 `PagesiftError` 一致，但不是 Error 实例（不采集调用栈）。
#[napi(object, object_from_js = false)]
pub struct BatchError {
    #[napi(ts_type = "ErrorCode")]
    pub code: String,
    pub message: String,
    #[napi(ts_type = "unknown")]
    pub details: Option<Value>,
    /// 限流时建议多少秒后再试。
    pub retry_after: Option<u32>,
}

impl From<ApiError> for BatchError {
    fn from(e: ApiError) -> Self {
        Self {
            code: e.code.as_str().to_string(),
            message: e.message,
            details: e.details,
            retry_after: e.retry_after.map(|s| s.min(u32::MAX.into()) as u32),
        }
    }
}
