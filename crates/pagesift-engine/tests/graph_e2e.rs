use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use pagesift_api::types::{CrawlRequest, CrawlSummary, Document, Format, ScrapeOptions};
use pagesift_api::{ApiError, CrawlSink};
use pagesift_engine::{Scraper, ScraperConfig};
use pagesift_graph::{
    ConflictSeverity, GraphOptions, GraphSummary, PageGraphDelta, ProvenanceEdge, SourceKind,
    StreamingGraphBuilder, VersionChangeKind,
};
use pagesift_sink::out::{DirOut, PageOut, WarcOut};
use pagesift_store::Store;

fn test_scraper(store: Option<Store>) -> Scraper {
    let mut cfg = ScraperConfig {
        store,
        ..Default::default()
    };
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    Scraper::new(cfg).expect("test scraper")
}

fn minimal_pdf(lines: &[&str], title: &str) -> Vec<u8> {
    let mut stream = String::from("BT\n/F1 12 Tf\n");
    for (idx, line) in lines.iter().enumerate() {
        let y = 720 - (idx as i32) * 50;
        stream.push_str(&format!("1 0 0 1 72 {y} Tm ({line}) Tj\n"));
    }
    stream.push_str("ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Count 1 /Kids [5 0 R] >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Title ({title}) >>"),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 6 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{obj}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, content) in entries {
        let mut blk = vec![0u8; 512];
        blk[..name.len()].copy_from_slice(name.as_bytes());
        blk[100..108].copy_from_slice(b"0000644\0");
        blk[108..116].copy_from_slice(b"0000000\0");
        blk[116..124].copy_from_slice(b"0000000\0");
        blk[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
        blk[136..148].copy_from_slice(b"00000000000\0");
        blk[148..156].copy_from_slice(b"        ");
        blk[156] = b'0';
        blk[257..263].copy_from_slice(b"ustar\0");
        blk[263..265].copy_from_slice(b"00");
        let sum: u32 = blk.iter().map(|&b| b as u32).sum();
        blk[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        tar.extend_from_slice(&blk);
        tar.extend_from_slice(content);
        let rem = content.len() % 512;
        if rem != 0 {
            tar.resize(tar.len() + (512 - rem), 0);
        }
    }
    tar.resize(tar.len() + 1024, 0);
    pagesift_compress::compress(&tar, 3)
}

#[derive(Default)]
struct RecordingSink {
    pages: Mutex<Vec<Document>>,
    deltas: Mutex<Vec<PageGraphDelta>>,
    summaries: Mutex<Vec<GraphSummary>>,
    errors: Mutex<Vec<(String, String)>>,
}

impl CrawlSink for RecordingSink {
    fn page(&self, doc: Document) -> pagesift_api::BoxFuture<'_, ()> {
        self.pages.lock().unwrap().push(doc);
        Box::pin(async {})
    }
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> pagesift_api::BoxFuture<'a, ()> {
        self.errors
            .lock()
            .unwrap()
            .push((url.to_string(), err.message));
        Box::pin(async {})
    }
    fn total(&self, _total: usize) -> pagesift_api::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancelled(&self) -> pagesift_api::BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn summary(&self, _summary: CrawlSummary) -> pagesift_api::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn graph_delta(&self, delta: PageGraphDelta) -> pagesift_api::BoxFuture<'_, ()> {
        self.deltas.lock().unwrap().push(delta);
        Box::pin(async {})
    }
    fn graph_summary(&self, summary: GraphSummary) -> pagesift_api::BoxFuture<'_, ()> {
        self.summaries.lock().unwrap().push(summary);
        Box::pin(async {})
    }
}

#[tokio::test]
async fn corporate_crawl_and_archive_corroborate_and_surface_conflicts() {
    let pdf_bytes = minimal_pdf(
        &["SKU: W-100", "Name: Acme Pro Widget", "Price: $129.00"],
        "Acme Widget Spec",
    );
    let archive_bytes = tar_zst(&[
        (
            "catalog.csv",
            b"sku,name,price\nW-100,Acme Pro Widget,$119.00\n",
        ),
        (
            "details.json",
            br#"{"sku":"W-100","name":"Acme Pro Widget","warranty":"24 months"}"#,
        ),
    ]);

    let pdf_arc = Arc::new(pdf_bytes);
    let zip_arc = Arc::new(archive_bytes);
    let app = Router::new()
        .route(
            "/",
            get(|| async {
                Html(
                    r#"<!doctype html><html><head><title>Acme Home</title></head><body><main>
                    <h1>Acme Portal</h1>
                    <a href="/product-a">Product A</a>
                    <a href="/product-b">Product B</a>
                    <a href="/spec.pdf">PDF Spec</a>
                    </main></body></html>"#,
                )
            }),
        )
        .route(
            "/product-a",
            get(|| async {
                Html(
                    r#"<!doctype html><html><head>
                    <title>Acme Pro Widget</title>
                    <link rel="canonical" href="/product-a">
                    <meta property="og:title" content="Acme Pro Widget">
                    <script type="application/ld+json">
                    {"@context":"https://schema.org","@type":"Product","sku":"W-100","name":"Acme Pro Widget","offers":{"@type":"Offer","price":"99.00","priceCurrency":"USD"}}
                    </script>
                    </head><body><main>
                    <h1>Acme Pro Widget</h1>
                    <div itemscope itemtype="https://schema.org/Product">
                      <span itemprop="name">Acme Pro Widget</span>
                      <span class="price">$99.00</span>
                    </div>
                    <p>See the <a href="/product-c">overview sheet</a> or download <a href="/bundle.tar.zst">bundle</a>.</p>
                    </main></body></html>"#,
                )
            }),
        )
        .route(
            "/product-b",
            get(|| async {
                Html(
                    r#"<!doctype html><html><head><title>Related Line</title></head><body><main>
                    <p>Compare with <a href="/product-c">shared overview</a>.</p>
                    </main></body></html>"#,
                )
            }),
        )
        .route(
            "/product-c",
            get(|| async {
                Html("<html><head><title>Overview</title></head><body><main><p>Common spec overview.</p></main></body></html>")
            }),
        )
        .route(
            "/spec.pdf",
            get(move || {
                let bytes = Arc::clone(&pdf_arc);
                async move {
                    let mut headers = HeaderMap::new();
                    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
                    headers.insert(
                        header::LINK,
                        HeaderValue::from_static("</canonical/spec.pdf>; rel=\"canonical\""),
                    );
                    (StatusCode::OK, headers, (*bytes).clone())
                }
            }),
        )
        .route(
            "/bundle.tar.zst",
            get(move || {
                let bytes = Arc::clone(&zip_arc);
                async move {
                    let mut headers = HeaderMap::new();
                    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zstd"));
                    (StatusCode::OK, headers, (*bytes).clone())
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");

    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(store_dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let sink = Arc::new(RecordingSink::default());
    let mut req: CrawlRequest = serde_json::from_value(serde_json::json!({
        "url": format!("{base}/"),
        "limit": 16,
        "crawlEntireDomain": true,
    }))
    .unwrap();
    req.graph = Some(GraphOptions::enabled());
    req.scrape_options = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        ..Default::default()
    };
    scraper.crawl(&req, sink.clone()).await.expect("crawl ok");

    let pages = sink.pages.lock().unwrap().clone();
    assert!(pages.len() >= 5, "expected >=5 pages, got {}", pages.len());
    let crawl_deltas = sink.deltas.lock().unwrap().clone();
    let crawl_summary = sink
        .summaries
        .lock()
        .unwrap()
        .pop()
        .expect("crawl graph summary");

    let target_c_key = format!("http://{addr}/product-c");
    let parent_edges: Vec<&str> = crawl_summary
        .citations
        .iter()
        .filter(|e| e.to_resource_key == target_c_key)
        .map(|e| e.from_resource_key.as_str())
        .collect();
    assert!(
        parent_edges.iter().any(|u| u.ends_with("/product-a"))
            && parent_edges.iter().any(|u| u.ends_with("/product-b")),
        "multi-parent citations must keep both product-a and product-b: {parent_edges:?}"
    );
    assert!(
        crawl_summary.provenance.iter().any(|p| matches!(
            p,
            ProvenanceEdge::CanonicalizesTo { canonical_url, .. }
            if canonical_url.ends_with("/canonical/spec.pdf")
        )),
        "PDF Link header canonical missing from provenance"
    );

    // Also scrape `/bundle.tar.zst` and unify everything through `StreamingGraphBuilder` + write DirOut & WarcOut
    let mut archive_opts = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    };
    let archive_doc = scraper
        .scrape(&format!("{base}/bundle.tar.zst"), &archive_opts)
        .await
        .expect("archive scrape");
    let archive_slice = archive_doc.graph.as_ref().expect("archive slice");
    assert!(
        archive_slice
            .member_deltas
            .iter()
            .any(|d| matches!(&d.node.source_kind, SourceKind::ArchiveMember { member_path, .. } if member_path == "catalog.csv")),
        "tar.zst CSV member missing"
    );
    assert!(
        archive_slice
            .member_deltas
            .iter()
            .any(|d| matches!(&d.node.source_kind, SourceKind::ArchiveMember { member_path, .. } if member_path == "details.json")),
        "tar.zst JSON member missing"
    );

    let out_dir = tempfile::tempdir().unwrap();
    let dir_out = DirOut::create(out_dir.path().join("dir")).unwrap();
    let warc_path = out_dir.path().join("crawl.warc");
    let warc_out = WarcOut::create(&warc_path).unwrap();

    let mut unified = StreamingGraphBuilder::new(GraphOptions::enabled());
    for d in &crawl_deltas {
        unified.ingest_delta(d.clone());
        dir_out.graph_delta(d).unwrap();
    }
    for doc in pages.iter() {
        dir_out.page(doc).unwrap();
        warc_out.page(doc).unwrap();
    }
    if let Some(root_delta) = archive_doc
        .graph
        .as_ref()
        .map(|s| pagesift_engine::graph::page_delta_from_document(&archive_doc, s))
    {
        unified.ingest_delta(root_delta);
        for m in &archive_slice.member_deltas {
            unified.ingest_delta(m.clone());
            dir_out.graph_delta(m).unwrap();
        }
    }
    let full_summary = unified.finalize();
    dir_out.graph_summary(&full_summary).unwrap();
    dir_out.finish().unwrap();
    warc_out.graph_summary(&full_summary).unwrap();
    warc_out.finish().unwrap();

    assert!(
        full_summary
            .fact_clusters
            .iter()
            .any(|f| f.subject_key == "sku:W-100"
                && f.normalized_field == "name"
                && f.distinct_sources >= 3),
        "expected cross-format corroboration across HTML, PDF, and tar.zst members: {:?}",
        full_summary.fact_clusters
    );
    let price_conflict = full_summary
        .conflicts
        .iter()
        .find(|c| c.subject_key == "sku:W-100" && c.normalized_field == "price")
        .expect("price conflict must be recorded");
    assert_eq!(price_conflict.severity, ConflictSeverity::Medium);
    assert!(
        price_conflict.variants.len() >= 3,
        "expected all 3 conflicting prices ($99 HTML, $129 PDF, $119 CSV) preserved: {:?}",
        price_conflict.variants
    );

    let graph_jsonl = std::fs::read_to_string(out_dir.path().join("dir/graph.jsonl")).unwrap();
    assert!(graph_jsonl.lines().count() >= 5);
    assert!(out_dir.path().join("dir/graph_summary.json").exists());
    let warc_text = String::from_utf8_lossy(&std::fs::read(&warc_path).unwrap()).into_owned();
    assert!(warc_text.contains("application/vnd.pagesift.graph+json"));
    assert!(warc_text.contains("application/vnd.pagesift.graph-summary+json"));

    // Verify extract + graph evidence alignment works without markdown in requested formats
    archive_opts.formats = vec![
        serde_json::from_value::<Format>(serde_json::json!({
            "type": "extract",
            "schema": {
                "baseSelector": "main",
                "fields": [{"name": "price", "selector": ".price", "type": "text"}]
            }
        }))
        .unwrap(),
        Format::new("graph"),
    ];
    let extracted = scraper
        .scrape(&format!("{base}/product-a"), &archive_opts)
        .await
        .expect("extract+graph");
    assert!(extracted.markdown.is_none());
    let eslice = extracted.graph.as_ref().unwrap();
    assert!(
        eslice
            .evidence
            .iter()
            .any(|ev| ev.verified && ev.excerpt.contains("$99.00")),
        "expected verified extract EvidenceSpan: {:?}",
        eslice.evidence
    );

    // Default opt-in guarantee: opts.graph = None leaves doc.graph == None, still extracts canonical_url
    let default_opts = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        ..Default::default()
    };
    let plain = scraper
        .scrape(&format!("{base}/product-a"), &default_opts)
        .await
        .expect("default scrape");
    assert!(plain.graph.is_none());
    assert_eq!(
        plain.metadata.canonical_url.as_deref(),
        Some(format!("{base}/product-a").as_str())
    );
}

#[tokio::test]
async fn persistent_versions_bridge_redirects_noise_and_substantive_edits() {
    #[derive(Clone, Copy)]
    enum Phase {
        V1Old,
        V2Moved,
        V3NoiseFooter,
        V4PriceBump,
    }
    let phase = Arc::new(Mutex::new(Phase::V1Old));
    let p_old = Arc::clone(&phase);
    let p_new = Arc::clone(&phase);

    let app = Router::new()
        .route(
            "/old",
            get(move || {
                let p = *p_old.lock().unwrap();
                async move {
                    match p {
                        Phase::V1Old => Html(
                            "<html><body><main><h1>Widget</h1><p>Current price: $99.00</p><p>Last updated 2025-01-01</p></main></body></html>",
                        )
                        .into_response(),
                        _ => Redirect::permanent("/new").into_response(),
                    }
                }
            }),
        )
        .route(
            "/new",
            get(move || {
                let p = *p_new.lock().unwrap();
                async move {
                    let body = match p {
                        Phase::V1Old | Phase::V2Moved => {
                            "<html><body><main><h1>Widget</h1><p>Current price: $99.00</p><p>Last updated 2025-01-01</p></main></body></html>"
                        }
                        Phase::V3NoiseFooter => {
                            "<html><body><main><h1>Widget</h1><p>Current price: $99.00</p><p>Last updated 2026-11-14</p></main></body></html>"
                        }
                        Phase::V4PriceBump => {
                            "<html><body><main><h1>Widget</h1><p>Current price: $149.00</p><p>Last updated 2026-11-14</p></main></body></html>"
                        }
                    };
                    Html(body).into_response()
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let old_url = format!("http://{addr}/old");
    let new_url = format!("http://{addr}/new");

    let store_dir = tempfile::tempdir().unwrap();
    let opts = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    };

    {
        let store = Store::open_dir(store_dir.path()).unwrap();
        let scraper = test_scraper(Some(store));

        let d1 = scraper.scrape(&old_url, &opts).await.unwrap();
        assert!(d1.graph.as_ref().unwrap().version.is_none());

        *phase.lock().unwrap() = Phase::V2Moved;
        let d2 = scraper.scrape(&old_url, &opts).await.unwrap();
        let e2 = d2
            .graph
            .as_ref()
            .unwrap()
            .version
            .as_ref()
            .expect("v2 edge");
        assert_eq!(e2.change_kind, VersionChangeKind::UrlMoved);

        *phase.lock().unwrap() = Phase::V3NoiseFooter;
        let d3 = scraper.scrape(&new_url, &opts).await.unwrap();
        let e3 = d3
            .graph
            .as_ref()
            .unwrap()
            .version
            .as_ref()
            .expect("v3 edge");
        assert_eq!(e3.change_kind, VersionChangeKind::NoiseOnly);

        *phase.lock().unwrap() = Phase::V4PriceBump;
        let d4 = scraper.scrape(&new_url, &opts).await.unwrap();
        let e4 = d4
            .graph
            .as_ref()
            .unwrap()
            .version
            .as_ref()
            .expect("v4 edge");
        assert_eq!(e4.change_kind, VersionChangeKind::Substantive);
    }

    // Reopen Store from disk to prove persistence across process restarts
    let reopened = Store::open_dir(store_dir.path()).unwrap();
    let gs = reopened.graph();
    let (canon, versions, edges) = gs.history(&old_url).unwrap();
    assert_eq!(canon, new_url);
    assert_eq!(
        versions.len(),
        4,
        "expected 4 unified versions: {versions:?}"
    );
    assert_eq!(edges.len(), 3);
    assert_eq!(edges[0].change_kind, VersionChangeKind::UrlMoved);
    assert_eq!(edges[1].change_kind, VersionChangeKind::NoiseOnly);
    assert_eq!(edges[2].change_kind, VersionChangeKind::Substantive);

    let latest = gs
        .latest_version_for_urls(&[old_url.as_str()])
        .unwrap()
        .unwrap();
    assert_eq!(latest.url, new_url);
    let _map: HashMap<String, String> = HashMap::new();
}

#[tokio::test]
async fn changed_block_ids_resolve_to_doc_block_ids_with_canonical() {
    let phase = Arc::new(Mutex::new(0usize));
    let phase_r = Arc::clone(&phase);
    let app = Router::new().route(
        "/item",
        get(move || {
            let p = *phase_r.lock().unwrap();
            async move {
                if p == 0 {
                    Html(
                        "<html><head><link rel=\"canonical\" href=\"/item-canonical\"></head>\
                         <body><h1>Product Spec</h1>\
                         <p>Alpha baseline paragraph stays untouched across revisions.</p>\
                         <p>Beta price starts at $100 with standard warranty coverage.</p></body></html>",
                    )
                } else {
                    Html(
                        "<html><head><link rel=\"canonical\" href=\"/item-canonical\"></head>\
                         <body><h1>Product Spec</h1>\
                         <p>Alpha baseline paragraph stays untouched across revisions.</p>\
                         <p>Beta price rises to $150 with extended warranty coverage.</p></body></html>",
                    )
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/item");

    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(store_dir.path()).unwrap();
    let scraper = test_scraper(Some(store));
    let opts = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    };

    let d1 = scraper.scrape(&url, &opts).await.expect("v1");
    let s1 = d1.graph.as_ref().expect("s1");
    assert_eq!(s1.resource_key, format!("http://{addr}/item-canonical"));
    assert!(!s1.blocks.is_empty());

    *phase.lock().unwrap() = 1;
    let d2 = scraper.scrape(&url, &opts).await.expect("v2");
    let s2 = d2.graph.as_ref().expect("s2");
    let edge = s2.version.as_ref().expect("v2 edge");
    assert!(!edge.changed_block_ids.is_empty());
    for cid in &edge.changed_block_ids {
        let matched = s2
            .blocks
            .iter()
            .find(|b| &b.block_id == cid && b.doc_id == s2.doc_id);
        assert!(
            matched.is_some(),
            "changed_block_id {cid} missing from slice.blocks under doc_id {}",
            s2.doc_id
        );
    }
}

#[tokio::test]
async fn query_filter_and_chunking_keep_graph_coordinates_aligned() {
    use pagesift_graph::EvidenceAlignment;

    let app = Router::new().route(
        "/offer",
        get(|| async {
            Html(
                "<html><head>\
                 <title>Acme Widget Overview</title>\
                 <meta name=\"description\" content=\"Leading overview description\">\
                 </head><body>\
                 <h1>Company Overview</h1>\
                 <p>Founded in 2010 in Osaka with global headquarters and overview story.</p>\
                 <h1>Enterprise Pricing Tiers</h1>\
                 <p>Enterprise plan costs $299 per month with dedicated SLA support and billing.</p>\
                 </body></html>",
            )
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/offer");

    let scraper = test_scraper(None);

    // 1) QueryFilter without chunking
    let opts_filtered = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        graph: Some(GraphOptions::enabled()),
        query_filter: Some(
            serde_json::from_value(serde_json::json!({
                "query": "Enterprise Pricing Tiers $299 SLA",
                "minTokens": 1,
                "relative": 0.9
            }))
            .unwrap(),
        ),
        ..Default::default()
    };
    let doc1 = scraper
        .scrape(&url, &opts_filtered)
        .await
        .expect("filtered");
    let md1 = doc1.markdown.as_deref().unwrap_or("");
    assert!(
        md1.contains("$299"),
        "expected pricing kept in filtered md: {md1}"
    );
    assert!(
        !md1.contains("Osaka"),
        "expected prefix overview dropped by queryFilter: {md1}"
    );
    let g1 = doc1.graph.as_ref().expect("g1");
    assert!(!g1.blocks.is_empty());
    for b in &g1.blocks {
        assert!(b.start <= b.end && b.end <= md1.len());
        assert!(md1.is_char_boundary(b.start) && md1.is_char_boundary(b.end));
    }
    for claim in &g1.claims {
        let ev = &claim.evidence;
        if let (Some(s), Some(e)) = (ev.start, ev.end) {
            assert!(s <= e && e <= md1.len());
            assert!(md1.is_char_boundary(s) && md1.is_char_boundary(e));
            let bid = ev.block_id.as_ref().expect("aligned block_id");
            assert!(g1.blocks.iter().any(|b| &b.block_id == bid));
        } else {
            assert!(ev.block_id.is_none());
            assert!(ev.chunk_idx.is_none());
            assert!(matches!(
                ev.alignment,
                EvidenceAlignment::Unaligned | EvidenceAlignment::MetadataIsland
            ));
        }
    }
    assert_eq!(g1.resource_key, url);

    // 2) QueryFilter combined with chunking
    let mut opts_chunked = opts_filtered;
    opts_chunked.chunking = Some(serde_json::json!({ "type": "recursive", "size": 90 }));
    let doc2 = scraper.scrape(&url, &opts_chunked).await.expect("chunked");
    let md2 = doc2.markdown.as_deref().unwrap_or("");
    let chunks2 = doc2.chunks.as_deref().expect("chunks2");
    assert!(!chunks2.is_empty());
    let g2 = doc2.graph.as_ref().expect("g2");
    for ev in g2
        .evidence
        .iter()
        .chain(g2.claims.iter().map(|c| &c.evidence))
    {
        if let (Some(s), Some(e)) = (ev.start, ev.end) {
            assert!(s <= e && e <= md2.len());
            if let Some(cidx) = ev.chunk_idx {
                let c = &chunks2[cidx];
                assert!(s < c.end && e > c.start, "span must overlap assigned chunk");
            }
        } else {
            assert!(ev.chunk_idx.is_none());
            assert!(ev.block_id.is_none());
        }
    }
}

#[tokio::test]
async fn oversize_and_evicted_snapshots_mark_version_diff_unavailable() {
    let phase = Arc::new(Mutex::new(0usize));
    let phase_r = Arc::clone(&phase);
    let app = Router::new().route(
        "/ver",
        get(move || {
            let p = *phase_r.lock().unwrap();
            async move {
                match p {
                    0 => Html("<html><body><p>Version 1 short body.</p></body></html>".to_string()),
                    1 => {
                        let pad = "Oversize payload word ".repeat(30);
                        Html(format!("<html><body><p>Version 2 {pad}</p></body></html>"))
                    }
                    2 => Html("<html><body><p>Version 3 short body.</p></body></html>".to_string()),
                    _ => Html("<html><body><p>Version 4 short body.</p></body></html>".to_string()),
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/ver");

    let store_dir = tempfile::tempdir().unwrap();
    let mut cfg = pagesift_store::Config::new(store_dir.path());
    cfg.snapshot_max_bytes = 96;
    cfg.max_bytes = Some(1);
    cfg.background = false;
    let store = Store::open(cfg).expect("open store");
    let scraper = test_scraper(Some(store.clone()));

    let opts = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    };

    // V1 normal (<96 bytes)
    let d1 = scraper.scrape(&url, &opts).await.expect("v1");
    assert!(d1.graph.as_ref().unwrap().version.is_none());

    // V2 oversize (>96 bytes): version commits + emits snapshot warning
    *phase.lock().unwrap() = 1;
    let d2 = scraper.scrape(&url, &opts).await.expect("v2");
    let w2 = d2
        .metadata
        .pagesift
        .as_ref()
        .map(|p| p.warnings.as_slice())
        .unwrap_or(&[]);
    assert!(
        w2.iter()
            .any(|w| w.contains("exceeds limit") || w.contains("too_large")),
        "expected oversize snapshot warning, got {w2:?}"
    );

    // V3 after oversize V2: MUST NOT fall back to V1 markdown; diff_stats is None and reason is reported
    *phase.lock().unwrap() = 2;
    let d3 = scraper.scrape(&url, &opts).await.expect("v3");
    let e3 = d3
        .graph
        .as_ref()
        .unwrap()
        .version
        .as_ref()
        .expect("v3 edge");
    assert_eq!(e3.diff_stats, None);
    assert!(
        e3.diff_unavailable_reason
            .as_deref()
            .is_some_and(|r| r.contains("snapshot_too_large")),
        "expected snapshot_too_large reason, got {:?}",
        e3.diff_unavailable_reason
    );

    // Evict V3 snapshot via Store::gc(), then V4 must report previous_snapshot_evicted (never V1!)
    let report = store.gc().expect("gc");
    assert!(report.evicted_entries >= 1);
    *phase.lock().unwrap() = 3;
    let d4 = scraper.scrape(&url, &opts).await.expect("v4");
    let e4 = d4
        .graph
        .as_ref()
        .unwrap()
        .version
        .as_ref()
        .expect("v4 edge");
    assert_eq!(e4.diff_stats, None);
    assert_eq!(
        e4.diff_unavailable_reason.as_deref(),
        Some("previous_snapshot_evicted")
    );
}

#[tokio::test]
async fn concurrent_scrapes_on_same_url_commit_linear_version_chain() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let seq = Arc::new(AtomicUsize::new(0));
    let seq_r = Arc::clone(&seq);
    let app = Router::new().route(
        "/race",
        get(move || {
            let n = seq_r.fetch_add(1, Ordering::Relaxed) + 1;
            async move {
                tokio::time::sleep(Duration::from_millis((n % 3) as u64 * 10)).await;
                Html(format!(
                    "<html><body><h1>Revision {n}</h1><p>Unique concurrent body #{n}.</p></body></html>"
                ))
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("http://{addr}/race");

    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open_dir(store_dir.path()).unwrap();
    let scraper = Arc::new(test_scraper(Some(store.clone())));

    let opts = ScrapeOptions {
        engine: Some("http".into()),
        max_age: Some(0),
        formats: vec![Format::new("markdown"), Format::new("graph")],
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    };

    let mut handles = Vec::new();
    for _ in 0..4 {
        let s = Arc::clone(&scraper);
        let u = url.clone();
        let o = opts.clone();
        handles.push(tokio::spawn(async move { s.scrape(&u, &o).await }));
    }
    for h in handles {
        h.await.unwrap().expect("concurrent scrape ok");
    }

    let gs = store.graph();
    let (_, versions, edges) = gs.history(&url).expect("history");
    assert_eq!(
        versions.len(),
        4,
        "expected 4 serialized versions: {versions:?}"
    );
    assert_eq!(edges.len(), 3, "expected 3 linear version edges: {edges:?}");
    for (idx, edge) in edges.iter().enumerate() {
        assert_eq!(
            edge.prev_doc_id.as_deref(),
            Some(versions[idx].doc_id.as_str())
        );
        assert_eq!(edge.curr_doc_id, versions[idx + 1].doc_id);
        assert!(versions[idx + 1].observed_at_ms > versions[idx].observed_at_ms);
    }
}

#[tokio::test]
async fn browser_engine_rejects_caller_ssrf_proxy_before_launch() {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = false;
    let scraper = Scraper::new(cfg).expect("ssrf scraper");

    let opts = ScrapeOptions {
        engine: Some("browser".into()),
        max_age: Some(0),
        proxy: Some("http://127.0.0.1:8080".into()),
        ..Default::default()
    };

    let err = scraper
        .scrape("https://example.com/", &opts)
        .await
        .expect_err("loopback caller proxy must be rejected");
    let msg = err.to_string().to_ascii_lowercase();
    assert!(
        msg.contains("loopback")
            || msg.contains("private")
            || msg.contains("proxy")
            || msg.contains("ssrf"),
        "unexpected error message: {err}"
    );
}
