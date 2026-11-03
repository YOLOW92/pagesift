//! 扫描页 OCR 的接入：用假引擎走完整的 HTTP 路径，看三种 `parsers` 模式、元数据和警告。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use pagesift::{Scraper, ScraperConfig};
use pagesift_pdf::{OcrInfo, OcrLine, PageOcr};
use serde_json::json;

use super::parse::{pdf, pdf_url, scanned_pdf, scrape};

#[derive(Debug, Default)]
struct Fake {
    calls: AtomicUsize,
    fail: bool,
}

impl PageOcr for Fake {
    fn recognize(&self, width: u32, height: u32, _gray: &[u8]) -> Result<Vec<OcrLine>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err("no models".into());
        }
        Ok(vec![OcrLine {
            x0: 0.0,
            y0: 0.0,
            x1: width as f32,
            y1: height as f32 / 2.0,
            text: "Recognized words".into(),
            score: 0.9,
            formula: false,
        }])
    }

    fn info(&self) -> OcrInfo {
        OcrInfo {
            engine: "fake".into(),
            model: "fake-small".into(),
            device: "cpu".into(),
            fallback: Some("no driver".into()),
            error: self.fail.then(|| "no models".to_string()),
        }
    }
}

fn app(ocr: Option<Arc<Fake>>) -> Router {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    cfg.ocr = ocr.map(|o| o as Arc<dyn PageOcr>);
    pagesift::api::router(Arc::new(Scraper::new(cfg).unwrap()), Default::default())
}

#[tokio::test]
async fn auto_mode_recognizes_scanned_pages_and_reports_it() {
    let fake = Arc::new(Fake::default());
    let app = app(Some(fake.clone()));
    let url = pdf_url(scanned_pdf()).await;
    let (s, v) = scrape(&app, &url, json!({"maxAge": 0})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Recognized words"),
        "{v}"
    );
    let meta = &v["data"]["metadata"]["pagesift"];
    assert_eq!(
        meta["ocr"],
        json!({"engine": "fake", "model": "fake-small", "device": "cpu", "pages": 1, "fallback": true})
    );
    let warnings = meta["warnings"].to_string();
    assert!(warnings.contains("gpu_fallback: no driver"), "{warnings}");
    assert!(
        !warnings.contains("scanned_pages_without_ocr"),
        "{warnings}"
    );
    assert_eq!(fake.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn text_pages_do_not_touch_the_engine() {
    let fake = Arc::new(Fake::default());
    let app = app(Some(fake.clone()));
    let url = pdf_url(pdf(&["Plain text page."], "T", false)).await;
    let (s, v) = scrape(&app, &url, json!({"maxAge": 0})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["data"]["metadata"]["pagesift"]["ocr"].is_null(), "{v}");
    assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn fast_mode_never_runs_ocr() {
    let fake = Arc::new(Fake::default());
    let app = app(Some(fake.clone()));
    let url = pdf_url(scanned_pdf()).await;
    let opts = json!({"maxAge": 0, "parsers": [{"type": "pdf", "mode": "fast"}]});
    let (s, v) = scrape(&app, &url, opts).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(fake.calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn ocr_mode_needs_the_server_to_have_it_on() {
    let url = pdf_url(scanned_pdf()).await;
    let opts = json!({"maxAge": 0, "parsers": [{"type": "pdf", "mode": "ocr"}]});
    let (s, v) = scrape(&app(None), &url, opts.clone()).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("UNSUPPORTED_OPTION")),
        "{v}"
    );
    let (s, v) = scrape(&app(Some(Arc::default())), &url, opts).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["metadata"]["pagesift"]["ocr"]["pages"], 1);
}

#[tokio::test]
async fn a_failing_engine_keeps_the_page_and_says_why() {
    let fake = Arc::new(Fake {
        fail: true,
        ..Default::default()
    });
    let url = pdf_url(scanned_pdf()).await;
    let (s, v) = scrape(&app(Some(fake)), &url, json!({"maxAge": 0})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let meta = &v["data"]["metadata"]["pagesift"];
    assert!(meta["ocr"].is_null(), "{v}");
    assert!(
        meta["warnings"]
            .to_string()
            .contains("ocr_failed: no models"),
        "{v}"
    );
}
