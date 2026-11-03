//! Word、Excel、PowerPoint、EPUB、CSV 端到端：`/v2/parse` 上传和 `/v2/scrape` 抓取同一份文件结果一致；
//! 坏文件、加密文件给出 4xx，XML 外部实体不展开（ZIP 炸弹等上限的测试在 pagesift-office 里）。

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

/// 最小的 ZIP 写入（条目都不压缩），测试里拼 OOXML/EPUB 用。
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let head = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend_from_slice(&[20, 0]);
            }
            h.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            if central {
                h.extend_from_slice(&[0; 10]);
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name.as_bytes());
            h
        };
        out.extend(head(0x0403_4b50, false));
        out.extend_from_slice(data);
        cd.extend(head(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

fn docx() -> Vec<u8> {
    let doc = format!(
        r#"<w:document {W}><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Annual report</w:t></w:r></w:p>
<w:p><w:r><w:t>Revenue grew.</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Year</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Total</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>2026</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles {W}><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style></w:styles>"#
    );
    let rels = r#"<Relationships><Relationship Id="s" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let core = r#"<cp:coreProperties xmlns:cp="cp" xmlns:dc="dc"><dc:title>Report 2026</dc:title></cp:coreProperties>"#;
    zip(&[
        ("word/document.xml", doc.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("docProps/core.xml", core.as_bytes()),
    ])
}

const DOCX_MD: &str =
    "# Annual report\n\nRevenue grew.\n\n| Year | Total |\n| --- | --- |\n| 2026 | 42 |\n";

fn app() -> Router {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::from_millis(1);
    pagesift::api::router(Arc::new(Scraper::new(cfg).unwrap()), Default::default())
}

/// 本地站点：`/f/<名字>` 返回给定字节和 Content-Type。
async fn site(files: Vec<(&'static str, &'static str, Vec<u8>)>) -> String {
    let mut app = Router::new();
    for (name, ctype, bytes) in files {
        app = app.route(
            &format!("/f/{name}"),
            get(move || {
                let bytes = bytes.clone();
                async move { ([(header::CONTENT_TYPE, ctype)], bytes) }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/f")
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn upload(app: &Router, name: &str, ctype: &str, data: &[u8]) -> (StatusCode, Value) {
    let b = "pagesiftOfficeBoundary";
    let mut body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {ctype}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    let req = Request::post("/v2/parse")
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .body(Body::from(body))
        .unwrap();
    send(app, req).await
}

async fn scrape(app: &Router, url: &str) -> (StatusCode, Value) {
    let req = Request::post("/v2/scrape")
        .header("content-type", "application/json")
        .body(Body::from(json!({"url": url, "maxAge": 0}).to_string()))
        .unwrap();
    send(app, req).await
}

#[tokio::test]
async fn docx_upload_and_scrape_agree() {
    let app = app();
    let bytes = docx();
    // 文件名写错、类型写成 octet-stream 也认得出（看 ZIP 里的部件）
    let (s, parsed) = upload(&app, "report.bin", "application/octet-stream", &bytes).await;
    assert_eq!(s, StatusCode::OK, "{parsed}");
    assert_eq!(parsed["data"]["markdown"], DOCX_MD);
    assert_eq!(parsed["data"]["metadata"]["title"], "Report 2026");
    assert_eq!(
        parsed["data"]["metadata"]["contentType"],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );
    assert!(parsed["data"]["metadata"]["pagesift"]["timings"]["docx"].is_number());
    let base = site(vec![("report.docx", "application/octet-stream", bytes)]).await;
    let (s, scraped) = scrape(&app, &format!("{base}/report.docx")).await;
    assert_eq!(s, StatusCode::OK, "{scraped}");
    assert_eq!(scraped["data"]["markdown"], DOCX_MD);
    assert_eq!(scraped["data"]["metadata"]["title"], "Report 2026");
}

#[tokio::test]
async fn csv_by_content_type_or_extension_but_not_html() {
    let app = app();
    let csv = b"name,qty\n\"Tea, green\",3\n".to_vec();
    let want = "| name | qty |\n| --- | --- |\n| Tea, green | 3 |\n";
    let base = site(vec![
        ("data.csv", "text/plain; charset=utf-8", csv.clone()),
        ("export", "text/csv", csv.clone()),
        (
            "page.csv",
            "text/html",
            b"<html><body><p>Not a CSV, an HTML page about CSV files.</p></body></html>".to_vec(),
        ),
    ])
    .await;
    for path in ["data.csv", "export"] {
        let (s, v) = scrape(&app, &format!("{base}/{path}")).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["data"]["markdown"], want, "{path}");
    }
    let (s, v) = scrape(&app, &format!("{base}/page.csv")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .contains("Not a CSV"),
        "{v}"
    );
    let (s, v) = upload(&app, "data.csv", "text/csv", &csv).await;
    assert_eq!(
        (s, v["data"]["markdown"].as_str()),
        (StatusCode::OK, Some(want))
    );
}

#[tokio::test]
async fn broken_encrypted_and_xxe_documents_are_handled() {
    let app = app();
    // 声称是 docx 的坏 ZIP、没有正文部件的 ZIP
    let mut cut = docx();
    cut.truncate(cut.len() / 2);
    let empty = zip(&[("word/other.xml", b"<x/>")]);
    // 加了口令的 Office 文件是 OLE 复合文件
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.resize(4096, 0);
    for (name, bytes, status, code) in [
        ("cut.docx", cut, StatusCode::BAD_REQUEST, "INVALID_REQUEST"),
        (
            "empty.docx",
            empty,
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
        ),
        (
            "locked.docx",
            ole.clone(),
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED_OPTION",
        ),
    ] {
        let (s, v) = upload(&app, name, "application/octet-stream", &bytes).await;
        assert_eq!((s, v["code"].as_str()), (status, Some(code)), "{name}: {v}");
    }
    // XML 外部实体：不展开，原样留在文字里，更不会去读本地文件
    let xxe = format!(
        r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/hostname">]><w:document {W}><w:body><w:p><w:r><w:t>&x;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let (s, v) = upload(
        &app,
        "xxe.docx",
        "application/octet-stream",
        &zip(&[("word/document.xml", xxe.as_bytes())]),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["markdown"], "&x;\n");
}

/// CSV 的字符集和网页一样处理：响应头的 `charset` 算数，没有声明时按正文探测（Shift_JIS、GBK），
/// 声明和正文对不上时以正文为准并给 `decode:` 警告；上传的文件也一样。
/// 0.80.0 把这些表格整页解成乱码，而且没有任何提示。
#[tokio::test]
async fn csv_charset_from_the_header_or_the_body() {
    let app = app();
    let ja = "番号,都市\n1,東京\n2,大阪\n";
    let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(ja);
    let sjis = sjis.into_owned();
    let zh: String = "编号,城市,备注\n".to_string()
        + &(0..40)
            .map(|i| format!("{i},北京上海广州深圳成都,订单 {i} 的备注信息很长很长\n"))
            .collect::<String>();
    let (gbk, _, _) = encoding_rs::GBK.encode(&zh);
    let gbk = gbk.into_owned();
    let base = site(vec![
        ("declared.csv", "text/csv; charset=shift_jis", sjis.clone()),
        ("detected.csv", "text/csv", gbk.clone()),
        ("lying.csv", "text/csv; charset=utf-8", gbk.clone()),
    ])
    .await;
    let (s, v) = scrape(&app, &format!("{base}/declared.csv")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["data"]["markdown"],
        "| 番号 | 都市 |\n| --- | --- |\n| 1 | 東京 |\n| 2 | 大阪 |\n"
    );
    assert!(
        v["data"]["metadata"]["pagesift"]["warnings"]
            .as_array()
            .is_none_or(|w| w.is_empty()),
        "{v}"
    );
    let (s, v) = scrape(&app, &format!("{base}/detected.csv")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let md = v["data"]["markdown"].as_str().unwrap();
    assert!(
        md.starts_with("| 编号 | 城市 | 备注 |\n")
            && md.contains("| 39 | 北京上海广州深圳成都 | 订单 39 的备注信息很长很长 |"),
        "{md}"
    );
    let (s, v) = scrape(&app, &format!("{base}/lying.csv")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .starts_with("| 编号 | 城市 | 备注 |\n"),
        "{v}"
    );
    let warnings = v["data"]["metadata"]["pagesift"]["warnings"].to_string();
    assert!(warnings.contains("decode:"), "{warnings}");
    // 上传：multipart 里文件的 Content-Type 带 charset
    let (s, v) = upload(&app, "data.csv", "text/csv; charset=shift_jis", &sjis).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["data"]["markdown"],
        "| 番号 | 都市 |\n| --- | --- |\n| 1 | 東京 |\n| 2 | 大阪 |\n"
    );
    let (s, v) = upload(&app, "data.csv", "text/csv", &gbk).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["data"]["markdown"]
            .as_str()
            .unwrap()
            .starts_with("| 编号 | 城市 | 备注 |\n"),
        "{v}"
    );
}
