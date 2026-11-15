use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use pagesift_engine::sync::SyncRunOptions;
use pagesift_engine::{Scraper, ScraperConfig};
use pagesift_store::{
    DataSourceSpec, FreshnessPolicy, ResourceRecord, Store, SyncBudget, SyncDelta,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;

pub fn test_scraper(store: Option<Store>) -> Scraper {
    let mut cfg = ScraperConfig::default();
    cfg.fetcher.env_proxy = false;
    cfg.fetcher.ssrf.allow_private_network = true;
    cfg.policy.rate.default_interval = Duration::ZERO;
    cfg.store = store;
    Scraper::new(cfg).expect("scraper builds cleanly for sync_e2e")
}

pub fn run_opts() -> SyncRunOptions {
    SyncRunOptions::default()
}

pub fn run_opts_forced() -> SyncRunOptions {
    SyncRunOptions {
        force_all: true,
        ..Default::default()
    }
}

pub fn sample_spec(id: &str, seed_url: String) -> DataSourceSpec {
    DataSourceSpec {
        id: id.to_string(),
        instance_id: String::new(),
        name: Some(format!("Source {id}")),
        seeds: vec![seed_url],
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        max_depth: 2,
        allow_subdomains: false,
        freshness: FreshnessPolicy {
            min_interval_secs: 0,
            max_interval_secs: 3600,
            default_interval_secs: 60,
            confirm_gone_after_404s: 2,
        },
        budget: SyncBudget {
            max_requests_per_tick: 64,
            max_browser_renders_per_tick: 8,
            max_wall_clock_ms: 30_000,
            target_items: None,
        },
        extract_schema: Some(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "sku": { "type": "string" },
                "price": { "type": "string" }
            }
        })),
        extract_prompt: None,
        required_fields: vec!["price".to_string(), "sku".to_string()],
        allow_browser: false,
        revision: 1,
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
    }
}

pub async fn spawn_server(app: Router) -> (String, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local test server");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), tx)
}

pub fn product_html(title: &str, sku: &str, price: &str, links: &[(&str, &str)]) -> String {
    let mut nav = String::new();
    for (href, label) in links {
        nav.push_str(&format!(r#"<a href="{href}">{label}</a> "#));
    }
    let ld = json!({
        "@context": "https://schema.org",
        "@type": "Product",
        "sku": sku,
        "name": title,
        "offers": {
            "@type": "Offer",
            "price": price,
            "priceCurrency": "USD"
        }
    });
    format!(
        r#"<!doctype html>
<html>
<head>
  <title>{title}</title>
  <script type="application/ld+json">{ld}</script>
</head>
<body>
  <main>
    <h1>{title}</h1>
    <p>Catalog specification for {title} (SKU {sku}). Current listed unit price is ${price} USD with full enterprise warranty and hardware support included for qualifying orders.</p>
    <p>Availability and verified documentation remain up to date across regional distribution centers.</p>
    <nav>{nav}</nav>
  </main>
</body>
</html>"#
    )
}

pub async fn tick_and_get(
    scraper: &Scraper,
    store: &Store,
    source_id: &str,
    opts: SyncRunOptions,
) -> (SyncDelta, Vec<ResourceRecord>) {
    let delta = scraper
        .run_sync_tick(source_id, opts)
        .await
        .expect("run_sync_tick succeeds");
    let inv = store
        .sync()
        .get_inventory(source_id)
        .expect("get_inventory succeeds");
    (delta, inv)
}

pub fn find_claim_val(
    claims: &[pagesift_graph::ClaimLifecycleRecord],
    field: &str,
) -> Option<Value> {
    claims
        .iter()
        .find(|c| c.claim.normalized_field == field)
        .map(|c| c.claim.raw_value.clone())
}

pub fn arc_state<T>(v: T) -> Arc<T> {
    Arc::new(v)
}
