//! `/v2/parse` 端到端：真实的 Scraper 解析上传的 PDF 和 HTML。同样的字节放到本地站点上用
//! `/v2/scrape` 抓一遍，正文和元数据要一致；坏文件、加密文件给出 4xx。

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use pagesift::{Scraper, ScraperConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

/// 本地站点上 `/doc.pdf` 的地址，返回给定的字节。
async fn pdf_url(bytes: Vec<u8>) -> String {
    format!("{}/doc.pdf", site(bytes).await)
}

/// 一页满版灰度图、没有文字层的“扫描件”。
fn scanned_pdf() -> Vec<u8> {
    let content = "q 612 0 0 792 0 0 cm BI /W 2 /H 2 /CS /G /BPC 8 ID \x7f\x7f\x7f\x7f EI Q";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// 一页一行字的 PDF：每页一个内容流，`/Info` 里有标题。`encrypt` 时加一个打不开的 /Encrypt。
fn pdf(pages: &[&str], title: &str, encrypt: bool) -> Vec<u8> {
    let n = pages.len();
    // 对象：1 Catalog，2 Pages，3 Font，4 Info，之后每页两个（Page、内容流），最后可能是 Encrypt
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        format!(
            "<< /Type /Pages /Count {n} /Kids [{}] >>",
            (0..n)
                .map(|i| format!("{} 0 R", 5 + 2 * i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!("<< /Title ({title}) >>"),
    ];
    for (i, text) in pages.iter().enumerate() {
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            6 + 2 * i
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    let zeros = "00".repeat(32);
    if encrypt {
        objs.push(format!(
            "<< /Filter /Standard /V 1 /R 2 /O <{zeros}> /U <{zeros}> /P -4 >>"
        ));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    let enc = if encrypt {
        format!(" /Encrypt {} 0 R /ID [<{zeros}> <{zeros}>]", objs.len())
    } else {
        String::new()
    };
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R{enc} >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

const HTML: &str = "<html><head><title>Upload test</title><meta name=\"description\" content=\"d\"></head>\
    <body><nav><a href=\"/home\">Home</a></nav><article><h1>Uploaded page</h1>\
    <p>This uploaded page has enough text in its article body to count as the main content.</p>\
    <p><a href=\"https://example.com/x\">absolute</a> and <a href=\"rel/y\">relative</a>.</p>\
    </article></body></html>";

fn app() -> Router {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    pagesift::api::router(Arc::new(Scraper::new(cfg).unwrap()), Default::default())
}

/// 本地站点：`/doc.pdf` 和 `/page.html` 返回给定的字节。
async fn site(pdf: Vec<u8>) -> String {
    let app = Router::new()
        .route(
            "/doc.pdf",
            get(move || {
                let pdf = pdf.clone();
                async move { ([(header::CONTENT_TYPE, "application/pdf")], pdf) }
            }),
        )
        .route(
            "/page.html",
            get(|| async { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], HTML) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn upload(
    app: &Router,
    name: &str,
    ctype: &str,
    data: &[u8],
    options: Value,
) -> (StatusCode, Value) {
    let b = "pagesiftTestBoundary";
    let mut body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {ctype}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(
        format!(
            "\r\n--{b}\r\nContent-Disposition: form-data; name=\"options\"\r\n\r\n{options}\r\n--{b}--\r\n"
        )
        .as_bytes(),
    );
    let req = Request::post("/v2/parse")
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .body(Body::from(body))
        .unwrap();
    send(app, req).await
}

async fn scrape(app: &Router, url: &str, options: Value) -> (StatusCode, Value) {
    let mut req = options;
    req["url"] = json!(url);
    let req = Request::post("/v2/scrape")
        .header("content-type", "application/json")
        .body(Body::from(req.to_string()))
        .unwrap();
    send(app, req).await
}

#[tokio::test]
async fn pdf_upload_matches_scrape_of_the_same_bytes() {
    let bytes = pdf(
        &["First page text.", "Second page text."],
        "Two pages",
        false,
    );
    let app = app();
    let base = site(bytes.clone()).await;
    let (s, scraped) = scrape(&app, &format!("{base}/doc.pdf"), json!({"maxAge": 0})).await;
    assert_eq!(s, StatusCode::OK, "{scraped}");
    let (s, parsed) = upload(&app, "doc.pdf", "application/pdf", &bytes, json!({})).await;
    assert_eq!(s, StatusCode::OK, "{parsed}");
    let (a, b) = (&scraped["data"], &parsed["data"]);
    assert!(
        b["markdown"]
            .as_str()
            .unwrap()
            .contains("Second page text."),
        "{b}"
    );
    assert_eq!(a["markdown"], b["markdown"]);
    for key in ["title", "numPages", "statusCode", "contentType"] {
        assert_eq!(a["metadata"][key], b["metadata"][key], "{key}");
    }
    assert_eq!(b["metadata"]["sourceURL"], "doc.pdf");
    assert_eq!(b["metadata"]["pagesift"]["engine"], "upload");
    assert!(b["metadata"]["pagesift"]["timings"]["pdf"].is_number());
}

#[tokio::test]
async fn pdf_parser_options_pages_and_max_pages() {
    let bytes = pdf(
        &["Alpha page.", "Beta page.", "Gamma page."],
        "Three",
        false,
    );
    let app = app();
    let (s, v) = upload(
        &app,
        "three.pdf",
        "application/pdf",
        &bytes,
        json!({"parsers": [{"type": "pdf", "mode": "fast", "maxPages": 2, "pages": true}]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let md = v["data"]["markdown"].as_str().unwrap();
    assert!(
        md.contains("Alpha page.") && md.contains("Beta page."),
        "{md}"
    );
    assert!(!md.contains("Gamma"), "{md}");
    let pages = v["data"]["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1]["pageNumber"], 2);
    assert!(
        pages[1]["markdown"]
            .as_str()
            .unwrap()
            .contains("Beta page.")
    );
    // 和 Firecrawl 一样：numPages 是转换的页数，totalPages 是文件的页数
    assert_eq!(v["data"]["metadata"]["numPages"], 2);
    assert_eq!(v["data"]["metadata"]["totalPages"], 3);
    let warnings = v["data"]["metadata"]["pagesift"]["warnings"].to_string();
    assert!(warnings.contains("max_pages"), "{warnings}");
    // 不要 pages 时没有这个字段
    let (_, v) = upload(&app, "three.pdf", "application/pdf", &bytes, json!({})).await;
    assert!(v["data"].get("pages").is_none());
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Gamma page.")
    );
}

#[tokio::test]
async fn bad_and_encrypted_pdfs_are_caller_errors() {
    let app = app();
    let mut broken = pdf(&["x"], "t", false);
    broken.truncate(40);
    let (s, v) = upload(&app, "bad.pdf", "application/pdf", &broken, json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "INVALID_REQUEST");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .starts_with("could not read PDF"),
        "{v}"
    );
    // 扩展名是 .pdf 但内容不是 PDF
    let (s, v) = upload(&app, "fake.pdf", "application/pdf", b"hello", json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "INVALID_REQUEST");
    let locked = pdf(&["secret"], "t", true);
    let (s, v) = upload(&app, "locked.pdf", "application/pdf", &locked, json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["code"], "UNSUPPORTED_OPTION");
    assert!(v["error"].as_str().unwrap().contains("encrypted"), "{v}");
}

#[tokio::test]
async fn html_upload_matches_scrape_of_the_same_bytes() {
    let app = app();
    let base = site(Vec::new()).await;
    let opts = json!({"formats": ["markdown", "links"], "maxAge": 0, "engine": "http"});
    let (s, scraped) = scrape(&app, &format!("{base}/page.html"), opts.clone()).await;
    assert_eq!(s, StatusCode::OK, "{scraped}");
    let opts = json!({"formats": ["markdown", "links"]});
    let (s, parsed) = upload(
        &app,
        "page.html",
        "text/html; charset=utf-8",
        HTML.as_bytes(),
        opts,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{parsed}");
    let (a, b) = (&scraped["data"], &parsed["data"]);
    let md = b["markdown"].as_str().unwrap();
    assert!(md.contains("# Uploaded page"), "{md}");
    assert!(!md.contains("Home"), "{md}");
    // 相对链接的基准不同（网址 vs file:///page.html），其余正文一致
    assert_eq!(
        a["markdown"]
            .as_str()
            .unwrap()
            .replace(&format!("{base}/"), "file:///"),
        md
    );
    for key in ["title", "description", "statusCode", "contentType"] {
        assert_eq!(a["metadata"][key], b["metadata"][key], "{key}");
    }
    // links 只收 http(s)：相对链接解析到 file:/// 后不进链接列表
    assert_eq!(b["links"], json!(["https://example.com/x"]));
    assert_eq!(b["metadata"]["sourceURL"], "page.html");
    // 猜出来的 file:///favicon.ico 不给
    assert!(b["metadata"].get("favicon").is_none(), "{b}");
}

#[tokio::test]
async fn html_upload_respects_only_main_content_and_raw_html() {
    let app = app();
    let (s, v) = upload(
        &app,
        "page.html",
        "text/html",
        HTML.as_bytes(),
        json!({"onlyMainContent": false, "formats": ["markdown", "rawHtml"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["data"]["markdown"].as_str().unwrap().contains("Home"));
    assert_eq!(v["data"]["rawHtml"], HTML);
}

#[tokio::test]
async fn scrape_honours_pdf_parsers_like_parse() {
    let bytes = pdf(
        &["Alpha page.", "Beta page.", "Gamma page."],
        "Three",
        false,
    );
    let app = app();
    let url = pdf_url(bytes.clone()).await;
    let opts = json!({"maxAge": 0, "parsers": [{"type": "pdf", "maxPages": 2, "pages": true}]});
    let (s, scraped) = scrape(&app, &url, opts.clone()).await;
    assert_eq!(s, StatusCode::OK, "{scraped}");
    let (s, parsed) = upload(&app, "doc.pdf", "application/pdf", &bytes, opts).await;
    assert_eq!(s, StatusCode::OK, "{parsed}");
    let (a, b) = (&scraped["data"], &parsed["data"]);
    assert!(!a["markdown"].as_str().unwrap().contains("Gamma"), "{a}");
    assert_eq!(a["markdown"], b["markdown"]);
    assert_eq!(a["pages"], b["pages"]);
    assert_eq!(a["pages"].as_array().unwrap().len(), 2);
    assert_eq!(a["metadata"]["numPages"], 2);
    assert_eq!(a["metadata"]["totalPages"], 3);
    // Firecrawl SDK 的写法：字符串 "pdf" 和 "image"
    let (s, v) = scrape(
        &app,
        &url,
        json!({"maxAge": 0, "parsers": ["pdf", "image"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Gamma page.")
    );
    // 写错的 parsers 在抓取前报 400
    let (s, v) = scrape(
        &app,
        &url,
        json!({"parsers": [{"type": "pdf", "maxPages": 0}]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = scrape(
        &app,
        &url,
        json!({"parsers": [{"type": "pdf", "mode": "ocr"}]}),
    )
    .await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("UNSUPPORTED_OPTION"))
    );
}

#[tokio::test]
async fn empty_parsers_return_the_pdf_as_base64() {
    let bytes = pdf(&["Raw page."], "Raw", false);
    let app = app();
    let url = pdf_url(bytes.clone()).await;
    let (s, v) = scrape(
        &app,
        &url,
        json!({"maxAge": 0, "parsers": [], "formats": ["markdown", "html"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let md = v["data"]["markdown"].as_str().unwrap();
    assert!(md.starts_with("JVBERi0"), "{md}"); // "%PDF-" 的 base64
    assert_eq!(md.len(), bytes.len().div_ceil(3) * 4);
    assert_eq!(v["data"]["html"], v["data"]["markdown"]);
    assert!(v["data"]["metadata"].get("numPages").is_none());
}

#[tokio::test]
async fn scrape_reports_bad_encrypted_and_scanned_pdfs_as_caller_errors() {
    let app = app();
    let mut broken = pdf(&["x"], "t", false);
    broken.truncate(40);
    let cases = [
        (
            broken,
            json!({}),
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
        ),
        (
            pdf(&["secret"], "t", true),
            json!({}),
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OPTION",
        ),
        (
            scanned_pdf(),
            json!({"parsers": [{"type": "pdf", "mode": "fast"}]}),
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OPTION",
        ),
    ];
    for (bytes, mut opts, status, code) in cases {
        opts["maxAge"] = json!(0);
        let url = pdf_url(bytes).await;
        let (s, v) = scrape(&app, &url, opts).await;
        assert_eq!((s, v["code"].as_str()), (status, Some(code)), "{v}");
    }
    // auto 模式下扫描件照常返回，带警告
    let url = pdf_url(scanned_pdf()).await;
    let (s, v) = scrape(&app, &url, json!({"maxAge": 0})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let warnings = v["data"]["metadata"]["pagesift"]["warnings"].to_string();
    assert!(warnings.contains("scanned PDF"), "{warnings}");
}
