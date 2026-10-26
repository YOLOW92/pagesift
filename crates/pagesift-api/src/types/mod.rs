//! Firecrawl v1/v2 request and response types.

pub mod request;
pub mod response;

pub use request::{
    AdaptiveOptions, BatchScrapeRequest, CrawlRequest, CrawlScope, ExtractRequest, Format,
    Location, MapRequest, QueryFilter, ScrapeOptions, ScrapeRequest, SearchRequest,
};
pub use response::{
    ChangeTracking, CrawlSummary, DocChunk, Document, JobError, JobErrors, JobStarted, JobStatus,
    MapLink, MapResponse, Metadata, PagesiftMeta, QueryFilterStats, ScrapeResponse,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_accept_strings_and_objects() {
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "formats": ["markdown", {"type": "screenshot", "fullPage": true}],
        }))
        .unwrap();
        assert_eq!(r.options.formats[0], Format::new("markdown"));
        assert_eq!(r.options.formats[1].kind, "screenshot");
        assert_eq!(r.options.formats[1].options["fullPage"], json!(true));
    }

    #[test]
    fn defaults_follow_firecrawl_and_unknown_fields_are_ignored() {
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "origin": "python-sdk",
            "integration": null,
            "zeroDataRetention": false,
        }))
        .unwrap();
        let o = r.options;
        assert_eq!(o.formats, vec![Format::new("markdown")]);
        assert!(o.only_main_content && o.block_ads && o.store_in_cache && o.remove_base64_images);
        assert_eq!(o.wait_for, 0);
        assert!(!o.ignore_robots_txt);
    }

    #[test]
    fn crawl_and_map_defaults() {
        let c: CrawlRequest =
            serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert_eq!((c.limit, c.sitemap.as_str()), (10_000, "include"));
        assert!(c.scrape_options.only_main_content);
        let m: MapRequest = serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert_eq!(m.limit, 5_000);
        assert!(m.include_subdomains && m.ignore_query_parameters);
    }

    #[test]
    fn query_filter_accepts_string_or_object() {
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com", "queryFilter": "refund policy",
        }))
        .unwrap();
        let f = r.options.query_filter.unwrap();
        assert_eq!(
            (
                f.query.as_str(),
                f.threshold,
                f.relative,
                f.top_k,
                f.headings
            ),
            ("refund policy", None, Some(0.5), None, true)
        );
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "queryFilter": {"query": "q", "threshold": 0.5, "topK": 3, "headings": false},
        }))
        .unwrap();
        let f = r.options.query_filter.unwrap();
        assert_eq!(
            (f.threshold, f.relative, f.top_k, f.headings),
            (Some(0.5), None, Some(3), false)
        );
        let r: ScrapeRequest = serde_json::from_value(json!({
            "url": "https://example.com",
            "queryFilter": {"query": "q", "maxTokens": 800},
        }))
        .unwrap();
        let f = r.options.query_filter.unwrap();
        assert_eq!((f.relative, f.max_tokens), (Some(0.5), Some(800)));
        assert!(ScrapeOptions::default().query_filter.is_none());
    }

    #[test]
    fn crawl_query_and_adaptive_options() {
        let c: CrawlRequest =
            serde_json::from_value(json!({"url": "https://example.com"})).unwrap();
        assert_eq!((c.query, c.adaptive), (None, None));
        let c: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com", "query": ["refund", "policy"], "adaptive": true,
        }))
        .unwrap();
        assert_eq!(c.query.as_deref(), Some("refund policy"));
        assert_eq!(c.adaptive, Some(AdaptiveOptions::default()));
        let c: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com", "query": "  ", "adaptive": {"patience": 2},
        }))
        .unwrap();
        assert_eq!(c.query, None, "blank query is no query");
        assert_eq!(c.adaptive.unwrap().patience, 2);
        let c: CrawlRequest = serde_json::from_value(json!({
            "url": "https://example.com", "query": null, "adaptive": false,
        }))
        .unwrap();
        assert_eq!((c.query, c.adaptive), (None, None));
    }

    #[test]
    fn change_tracking_serializes_firecrawl_fields() {
        let doc = Document {
            change_tracking: Some(ChangeTracking {
                previous_scrape_at: None,
                change_status: "new".into(),
                visibility: "visible".into(),
                diff: None,
                json: None,
                pagesift: None,
            }),
            ..Default::default()
        };
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            v["changeTracking"],
            json!({"previousScrapeAt": null, "changeStatus": "new", "visibility": "visible"})
        );
        assert!(
            serde_json::to_value(Document::default())
                .unwrap()
                .get("changeTracking")
                .is_none()
        );
    }

    #[test]
    fn metadata_serializes_firecrawl_names_and_extension() {
        let mut meta = Metadata {
            source_url: Some("https://example.com/a".into()),
            status_code: 200,
            proxy_used: "basic".into(),
            pagesift: Some(PagesiftMeta {
                content_hash: Some("ab".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        meta.extra
            .insert("ogImage".into(), json!("https://example.com/i.png"));
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(v["sourceURL"], "https://example.com/a");
        assert_eq!(v["statusCode"], 200);
        assert_eq!(v["proxyUsed"], "basic");
        assert_eq!(v["ogImage"], "https://example.com/i.png");
        assert_eq!(v["pagesift"]["contentHash"], "ab");
        assert!(v.get("title").is_none());
    }
}
