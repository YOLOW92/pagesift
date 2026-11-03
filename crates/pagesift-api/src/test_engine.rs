//! 模块测试用的假引擎：scrape 返回固定文档；给了闸门时每抓一页先等闸门放行一个许可。

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tokio::sync::Semaphore;
use tower::ServiceExt;

use crate::engine::{BoxFuture, Engine};
use crate::error::ApiError;
use crate::types::{Document, Metadata, ScrapeOptions};

pub(crate) struct FakeEngine {
    pub(crate) gate: Option<Arc<Semaphore>>,
    pub(crate) title: Option<String>,
    pub(crate) markdown: Option<String>,
}

impl FakeEngine {
    pub(crate) fn open() -> Self {
        Self {
            gate: None,
            title: None,
            markdown: None,
        }
    }
}

impl Engine for FakeEngine {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            if let Some(g) = &self.gate {
                g.acquire().await.expect("gate open").forget();
            }
            Ok(Document {
                markdown: Some(self.markdown.clone().unwrap_or_else(|| format!("# {url}"))),
                metadata: Metadata {
                    title: self.title.clone(),
                    source_url: Some(url.to_string()),
                    status_code: 200,
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }
}

/// 发一个请求，返回状态码和响应体的各个数据帧。
pub(crate) async fn frames(
    app: &axum::Router,
    req: Request<Body>,
) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<Vec<u8>>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, mut body) = resp.into_parts();
    let mut out = Vec::new();
    while let Some(f) = body.frame().await {
        if let Ok(d) = f.unwrap().into_data() {
            out.push(d.to_vec());
        }
    }
    (parts.status, parts.headers, out)
}
