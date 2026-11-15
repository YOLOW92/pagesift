//! 1.5 Loop sync & DataGraph promise adapters for Node `Scraper`.

use std::future::Future;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use serde_json::Value;

use crate::error::{self, Fallible};

pub(crate) fn spawn<'e, F, Fut>(
    env: &'e Env,
    ready: Fallible<Arc<pagesift::Scraper>>,
    request: Option<Value>,
    op: F,
) -> Result<PromiseRaw<'e, Value>>
where
    F: FnOnce(Arc<pagesift::Scraper>, Value) -> Fut + Send + 'static,
    Fut: Future<Output = Fallible<Value>> + Send + 'static,
{
    let req = request
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| Value::Object(Default::default()));
    let fut = async move {
        Ok(match ready {
            Ok(core) => op(core, req).await,
            Err(e) => Err(e),
        })
    };
    env.spawn_future_with_callback(fut, |env, out: Fallible<Value>| {
        out.map_err(|e| error::to_js(env, &e))
    })
}
