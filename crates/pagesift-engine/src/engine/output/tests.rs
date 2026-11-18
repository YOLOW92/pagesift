use super::{is_pdf, json_markdown, thin_basis_bytes, unsupported_content_type};

#[test]
fn thin_basis_ignores_inline_payloads() {
    let image = format!(
        "<p>short</p><img src=\"data:image/png;base64,{}\">",
        "A".repeat(100_000)
    );
    assert!(thin_basis_bytes(&image, "short") < 1_000);
    let style = format!("<STYLE>{}</STYLE><p>x</p>", "y".repeat(50_000));
    assert!(thin_basis_bytes(&style, "x") < 1_000);
    // 自定义元素 `<styled-div>` 不是 `<style>`，不得吞掉其后的 JS 脚本体积
    let styled_custom = format!(
        "<styled-div id=\"app\"></styled-div><script>{}</script>",
        "x".repeat(30_000)
    );
    assert_eq!(thin_basis_bytes(&styled_custom, ""), styled_custom.len());
    // `<style>` 内的 `url(data:...)` 只扣一次，不能重复扣减而抵消 `<script>` 壳体积
    let style_with_data_and_script = format!(
        "<style>.bg{{background:url(data:image/png;base64,{})}}</style><script>{}</script>",
        "A".repeat(30_000),
        "x".repeat(30_000)
    );
    assert!(
        thin_basis_bytes(&style_with_data_and_script, "") >= 30_000,
        "data: inside <style> must not be subtracted twice"
    );
    let next = format!(
        "<script id=\"__NEXT_DATA__\">{}</script><div id=\"app\"></div>",
        "x".repeat(50_000)
    );
    assert_eq!(thin_basis_bytes(&next, ""), next.len());
    let shell = format!("<div>{}</div>", "<span></span>".repeat(5_000));
    assert_eq!(thin_basis_bytes(&shell, ""), shell.len());
    let long = "word ".repeat(200);
    assert_eq!(thin_basis_bytes(&image, &long), image.len());
}

#[test]
fn json_responses_with_utf8_bom_preserve_parsed_value() {
    let payload = b"\xef\xbb\xbf{\"sku\":\"BOM-1\",\"price\":42}";
    let (md, val) = super::parse_json_markdown(Some("application/json"), payload)
        .expect("UTF-8 BOM JSON must parse");
    assert!(md.contains("\"sku\": \"BOM-1\""));
    assert_eq!(val["sku"], "BOM-1");
    assert_eq!(val["price"], 42);
}

#[test]
fn single_dom_warnings_and_traces_match_legacy_multi_pass() {
    let html = "<html><head>\0\
        <meta property=\"og:title\" content=\"Pro Widget\">\
        <script type=\"application/ld+json\">{\"@context\":\"https://schema.org\",\"@type\":\"Product\",\"sku\":\"W-9\",\"price\":\"99.00\"}</script>\
        <script type=\"application/ld+json\">{broken json</script>\
        </head><body>\
        <div itemscope itemtype=\"https://schema.org/Product\"><span itemprop=\"name\">Pro Widget</span></div>\
        <script id=\"__NEXT_DATA__\" type=\"application/json\">{\"props\":{\"pageProps\":{\"stock\":17}}}</script>\
        </body></html>";
    let url = Some("https://example.com/p/w9");
    let dom = pagesift_html::Html::parse(html);
    let (warnings, traces) = super::collect_dom_warnings_and_traces(html, &dom, true, url);
    let legacy_warnings = super::collect_html_text_warnings(html, None);
    let legacy_traces = crate::graph::evidence::collect_structured_html_traces(html, url);
    assert_eq!(warnings, legacy_warnings);
    assert_eq!(warnings.len(), 2);
    assert_eq!(traces, legacy_traces);
    assert!(traces.iter().any(|t| t.field_name == "stock"));
}

#[test]
fn json_responses_become_code_blocks() {
    let md = json_markdown(
        Some("application/json; charset=utf-8"),
        br#"{"html":"<h1>x</h1>","n":[1,2]}"#,
    )
    .unwrap();
    assert!(md.starts_with("```json\n{"), "{md}");
    assert!(md.contains("\"html\": \"<h1>x</h1>\""), "{md}");
    assert!(md.ends_with("\n```\n"), "{md}");
    let md = json_markdown(Some("application/ld+json"), br#"{"a":"```"}"#).unwrap();
    assert!(md.starts_with("````json"), "{md}");
    assert_eq!(json_markdown(Some("text/html"), b"{}"), None);
    assert_eq!(json_markdown(Some("application/json"), b"<html>"), None);
}

#[test]
fn rejects_binary_content_types() {
    for kind in [
        "image/png",
        "image/jpeg",
        "image/svg+xml",
        "audio/mpeg",
        "video/mp4",
        "font/woff2",
        "application/zip",
        "application/octet-stream",
    ] {
        assert_eq!(
            unsupported_content_type(Some(kind), b"\x89PNG\r\n"),
            Some(kind.to_string()),
            "{kind}"
        );
    }
}

#[test]
fn accepts_text_and_mislabeled_markup() {
    for kind in [
        "text/html; charset=utf-8",
        "text/plain",
        "application/xhtml+xml",
        "application/json",
        "application/rss+xml",
        "application/pdf",
    ] {
        assert_eq!(
            unsupported_content_type(Some(kind), b"anything"),
            None,
            "{kind}"
        );
    }
    assert_eq!(
        unsupported_content_type(Some("application/octet-stream"), b"<!doctype html><p>x"),
        None
    );
    assert_eq!(
        unsupported_content_type(Some("application/octet-stream"), b"  <html><p>x"),
        None
    );
    assert_eq!(unsupported_content_type(None, b"\x89PNG"), None);
    for kind in ["application/octet-stream", "binary/octet-stream"] {
        assert_eq!(
            unsupported_content_type(Some(kind), b"%PDF-1.7\n1 0 obj"),
            None
        );
        assert!(is_pdf(b"%PDF-1.7\n1 0 obj", Some(kind)));
    }
    // 头说是 PDF：正文是网页（拦截页、登录页）时按网页处理；不是网页的照样交给 PDF 解析
    let pdf = Some("application/pdf");
    assert!(!is_pdf(
        b"<!DOCTYPE html><html><body>Access denied</body></html>",
        pdf
    ));
    assert!(!is_pdf(b"\n  <html><body>x</body></html>", pdf));
    assert!(is_pdf(b"junk before the header %PDF-1.4", pdf));
    assert!(is_pdf(b"%PDF-1.4", Some("text/html")));
    assert_eq!(
        unsupported_content_type(Some("application/octet-stream"), b"\x00\x01binary"),
        Some("application/octet-stream".into())
    );
}

#[tokio::test]
async fn process_html_preserves_figure_captions_and_drops_noise() {
    let filler: String = (0..12)
        .map(|i| {
            format!(
                "<p>Paragraph {i}: detailed methodology discussion covering datasets, \
                 evaluation metrics, and empirical ablation stability across trials.</p>"
            )
        })
        .collect();
    let html = format!(
        "<html><head><title>Empirical Report</title>\
         <meta property=\"og:type\" content=\"article\"></head><body>\
         <nav><a href=\"/top\">TopNavigation chrome</a></nav>\
         <article><h1>Empirical Report</h1>{filler}\
         <figure class=\"wp-caption\"><img src=\"/fig/ablation.png\" alt=\"Ablation\">\
         <figcaption class=\"figure-caption\">Figure 2: Ablation latency distribution.</figcaption></figure>\
         <figure class=\"related\"><img src=\"/ad/promo.png\" alt=\"Promo\">\
         <figcaption>Recommended promo item</figcaption></figure>\
         </article></body></html>"
    );
    let base = super::Url::parse("https://example.org/report").unwrap();
    let opts = super::ScrapeOptions {
        formats: vec![
            pagesift_api::types::Format::new("markdown"),
            pagesift_api::types::Format::new("html"),
            pagesift_api::types::Format::new("images"),
        ],
        only_main_content: true,
        ..Default::default()
    };
    let (raw_doc, raw_html, _) = super::process_html(html, &base, Some(200), &opts)
        .await
        .unwrap();
    let out = super::document(raw_doc, &opts, raw_html);
    let md = out.markdown.as_deref().unwrap();
    assert!(
        md.contains("![Ablation](https://example.org/fig/ablation.png)"),
        "{md}"
    );
    assert!(
        md.contains("Figure 2: Ablation latency distribution."),
        "{md}"
    );
    assert!(!md.contains("TopNavigation chrome"), "{md}");
    assert!(!md.contains("Recommended promo item"), "{md}");
    assert!(
        out.html
            .as_deref()
            .is_some_and(|h| h.contains("Figure 2: Ablation latency distribution."))
    );
    assert!(out.images.as_ref().is_some_and(|imgs| {
        imgs.iter()
            .any(|i| i == "https://example.org/fig/ablation.png")
    }));
}
