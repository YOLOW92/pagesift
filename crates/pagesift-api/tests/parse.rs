//! `/v2/parse` 的协议层：multipart 解析、大小上限、选项校验、错误码。真实的 PDF/HTML 解析在
//! `crates/pagesift/tests/parse.rs`。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pagesift_api::types::{Document, Metadata, PdfParser, ScrapeOptions, Upload, UploadKind};
use pagesift_api::{ApiError, BoxFuture, Config, Engine, MAX_PARSE_FILE_BYTES, Timeouts, router};
use serde_json::Value;
use tower::ServiceExt;

/// 假引擎：把收到的文件和选项原样报回来。
struct Fake;

impl Engine for Fake {
    fn scrape<'a>(
        &'a self,
        _url: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async { Err(ApiError::invalid("not used")) })
    }

    fn parse<'a>(
        &'a self,
        file: Upload,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async move {
            let parser = PdfParser::from_parsers(opts.parsers.as_ref())?;
            let kind = match file.kind {
                UploadKind::Pdf => "pdf",
                UploadKind::Html => "html",
                UploadKind::Office(f) => f.extension(),
            };
            Ok(Document {
                markdown: opts.wants("markdown").then(|| {
                    format!(
                        "{kind} {} bytes, main={} maxPages={:?}",
                        file.bytes.len(),
                        opts.only_main_content,
                        parser.max_pages
                    )
                }),
                metadata: Metadata {
                    source_url: Some(file.filename),
                    content_type: file.content_type,
                    status_code: 200,
                    proxy_used: "basic".into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }
}

fn app(keys: &[&str]) -> axum::Router {
    router(
        Arc::new(Fake),
        Config {
            api_keys: keys.iter().map(|k| k.to_string()).collect(),
            ..Default::default()
        },
    )
}

const BOUNDARY: &str = "XpagesiftBoundary";

/// 一个 multipart 字段：(字段名, 文件名, Content-Type, 内容)。
type Part<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

fn multipart(parts: &[Part]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, ctype, data) in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let mut disp = format!("Content-Disposition: form-data; name=\"{name}\"");
        if let Some(f) = filename {
            disp.push_str(&format!("; filename=\"{f}\""));
        }
        body.extend_from_slice(disp.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(t) = ctype {
            body.extend_from_slice(format!("Content-Type: {t}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

fn request(body: Vec<u8>) -> Request<Body> {
    Request::post("/v2/parse")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header("content-length", body.len())
        .body(Body::from(body))
        .unwrap()
}

async fn parse(parts: &[Part<'_>]) -> (StatusCode, Value) {
    send(&app(&[]), request(multipart(parts))).await
}

const PDF: &[u8] = b"%PDF-1.7\n% fake\n";

#[tokio::test]
async fn pdf_upload_returns_scrape_shape() {
    let (status, v) = parse(&[
        ("file", Some("dir/report.pdf"), Some("application/pdf"), PDF),
        (
            "options",
            None,
            Some("application/json"),
            br#"{"formats":[{"type":"markdown"}],"onlyMainContent":false,"parsers":[{"type":"pdf","maxPages":2}],"timeout":300000,"origin":"python-sdk"}"#,
        ),
    ])
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["success"], true);
    assert_eq!(
        v["data"]["markdown"],
        format!("pdf {} bytes, main=false maxPages=Some(2)", PDF.len())
    );
    // 文件名只保留最后一段路径
    assert_eq!(v["data"]["metadata"]["sourceURL"], "report.pdf");
    assert_eq!(v["data"]["metadata"]["statusCode"], 200);
}

#[tokio::test]
async fn options_are_optional_and_default_like_firecrawl() {
    let (status, v) = parse(&[("file", Some("a.pdf"), None, PDF)]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["data"]["markdown"],
        format!("pdf {} bytes, main=true maxPages=None", PDF.len())
    );
}

#[tokio::test]
async fn file_type_comes_from_content_then_name_then_mime() {
    // 内容是 PDF，文件名不对也按 PDF
    let (s, v) = parse(&[("file", Some("blob.bin"), None, PDF)]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["data"]["markdown"].as_str().unwrap().starts_with("pdf"));
    let html = b"<html><body><p>hi</p></body></html>";
    let (s, v) = parse(&[("file", Some("page.htm"), None, html)]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["data"]["markdown"].as_str().unwrap().starts_with("html"));
    let (s, v) = parse(&[("file", Some("upload"), Some("text/html; charset=gbk"), html)]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["data"]["markdown"].as_str().unwrap().starts_with("html"));
    assert_eq!(
        v["data"]["metadata"]["contentType"],
        "text/html; charset=gbk"
    );
    // Office 文件按扩展名认（ZIP 坏了的话由引擎报 INVALID_REQUEST）
    let (s, v) = parse(&[("file", Some("a.docx"), None, b"PK\x03\x04")]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v["data"]["markdown"].as_str().unwrap().starts_with("docx"),
        "{v}"
    );
    // 认不出的类型拒绝（OpenDocument、旧版 .doc 不支持）
    for name in ["a.odt", "a.doc", "a.zip"] {
        let (s, v) = parse(&[("file", Some(name), None, b"PK\x03\x04")]).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{name}");
        assert_eq!(v["code"], "UNSUPPORTED_OPTION");
    }
}

#[tokio::test]
async fn missing_or_empty_file_is_invalid() {
    let (s, v) = parse(&[("options", None, None, b"{}")]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["code"], "INVALID_REQUEST");
    assert!(v["error"].as_str().unwrap().contains("'file'"), "{v}");
    let (s, v) = parse(&[("file", Some("a.pdf"), None, b"")]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["code"], "INVALID_REQUEST");
    let (s, v) = parse(&[
        ("file", Some("a.pdf"), None, PDF),
        ("file", Some("b.pdf"), None, PDF),
    ])
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["code"], "INVALID_REQUEST");
}

#[tokio::test]
async fn non_multipart_bodies_are_rejected_uniformly() {
    let req = Request::post("/v2/parse")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"file":"x"}"#))
        .unwrap();
    let (s, v) = send(&app(&[]), req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["success"], false);
    assert_eq!(v["code"], "INVALID_REQUEST");
    // 截断的 multipart（没有结束分隔行）
    let mut body = multipart(&[("file", Some("a.pdf"), None, PDF)]);
    body.truncate(body.len() - 10);
    let (s, v) = send(&app(&[]), request(body)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "INVALID_REQUEST");
}

#[tokio::test]
async fn options_must_be_a_json_object() {
    for (raw, code) in [
        (&b"{not json"[..], "INVALID_REQUEST"),
        (b"[1,2]", "INVALID_REQUEST"),
        (b"\"markdown\"", "INVALID_REQUEST"),
        (br#"{"timeout": 999999}"#, "INVALID_REQUEST"),
        (br#"{"formats": 5}"#, "INVALID_REQUEST"),
        (
            br#"{"parsers":[{"type":"pdf","mode":"ocr"}]}"#,
            "UNSUPPORTED_OPTION",
        ),
        (
            br#"{"parsers":[{"type":"pdf","blocks":true}]}"#,
            "UNSUPPORTED_OPTION",
        ),
        (
            br#"{"parsers":[{"type":"pdf","maxPages":0}]}"#,
            "INVALID_REQUEST",
        ),
        (br#"{"formats":["screenshot"]}"#, "UNSUPPORTED_OPTION"),
        (br#"{"formats":["changeTracking"]}"#, "UNSUPPORTED_OPTION"),
        (
            br#"{"actions":[{"type":"wait","milliseconds":1}]}"#,
            "UNSUPPORTED_OPTION",
        ),
        (br#"{"waitFor": 1000}"#, "UNSUPPORTED_OPTION"),
        (br#"{"mobile": true}"#, "UNSUPPORTED_OPTION"),
        (br#"{"proxy": "stealth"}"#, "UNSUPPORTED_OPTION"),
        (br#"{"engine": "browser"}"#, "UNSUPPORTED_OPTION"),
    ] {
        let (s, v) = parse(&[
            ("file", Some("a.pdf"), None, PDF),
            ("options", None, None, raw),
        ])
        .await;
        assert_eq!(
            s,
            StatusCode::BAD_REQUEST,
            "{}: {v}",
            String::from_utf8_lossy(raw)
        );
        assert_eq!(v["code"], code, "{}: {v}", String::from_utf8_lossy(raw));
    }
    // Firecrawl 允许的写法照收：proxy basic/auto、headers、未知字段
    let (s, v) = parse(&[
        ("file", Some("a.pdf"), None, PDF),
        (
            "options",
            None,
            None,
            br#"{"proxy":"auto","headers":{"x":"y"},"zeroDataRetention":false,"integration":null,"parsers":["pdf"]}"#,
        ),
    ])
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn oversized_upload_is_413_before_reading() {
    // Content-Length 声明超限：不读请求体就拒绝
    let req = Request::post("/v2/parse")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header("content-length", 60 * 1024 * 1024)
        .body(Body::from(multipart(&[("file", Some("a.pdf"), None, PDF)])))
        .unwrap();
    let (s, v) = send(&app(&[]), req).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(v["code"], "TOO_LARGE");
}

#[tokio::test]
async fn oversized_file_is_413_while_streaming() {
    // 不带 Content-Length（分块上传），读到超限时停下
    let big = vec![b'a'; MAX_PARSE_FILE_BYTES + 1];
    let body = multipart(&[("file", Some("a.pdf"), None, &big)]);
    let stream = futures_stream(body);
    let req = Request::post("/v2/parse")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(stream)
        .unwrap();
    let (s, v) = send(&app(&[]), req).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["code"], "TOO_LARGE");
    // 正好 50 MB 可以
    let exact = vec![b'a'; MAX_PARSE_FILE_BYTES];
    let (s, v) = parse(&[("file", Some("a.pdf"), None, &exact)]).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// 1 MB 一块的流式请求体，没有 Content-Length。
fn futures_stream(body: Vec<u8>) -> Body {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for c in body.chunks(1024 * 1024) {
        tx.send(axum::body::Bytes::copy_from_slice(c)).unwrap();
    }
    Body::new(Chunks(rx))
}

/// 从通道里一块块吐出的请求体；发送端还开着而没有数据时就一直等。
struct Chunks(tokio::sync::mpsc::UnboundedReceiver<axum::body::Bytes>);

impl http_body::Body for Chunks {
    type Data = axum::body::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.0
            .poll_recv(cx)
            .map(|c| c.map(|c| Ok(http_body::Frame::data(c))))
    }
}

#[tokio::test]
async fn auth_applies_to_parse() {
    let app = app(&["k1"]);
    let body = multipart(&[("file", Some("a.pdf"), None, PDF)]);
    let (s, v) = send(&app, request(body.clone())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["code"], "UNAUTHORIZED");
    let mut req = request(body);
    req.headers_mut()
        .insert("authorization", "Bearer k1".parse().unwrap());
    let (s, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn upload_body_deadline_is_separate() {
    let t = Timeouts::default();
    assert!(t.upload_body > t.body);
    // 请求体迟迟不来：超过上传时限回 408
    let app = router(
        Arc::new(Fake),
        Config {
            timeouts: Timeouts {
                body: Duration::from_millis(100),
                upload_body: Duration::from_millis(300),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let head = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.pdf\"\r\n\r\n%PDF-"
    );
    tx.send(head.into()).unwrap();
    let body = Body::new(Chunks(rx));
    let req = Request::post("/v2/parse")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .unwrap();
    let started = std::time::Instant::now();
    let (s, v) = send(&app, req).await;
    assert_eq!(s, StatusCode::REQUEST_TIMEOUT, "{v}");
    assert_eq!(v["code"], "TIMEOUT");
    // 用的是上传时限（300 ms），不是普通请求体的 100 ms
    assert!(started.elapsed() >= Duration::from_millis(280));
    drop(tx);
}
