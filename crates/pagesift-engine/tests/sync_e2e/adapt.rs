use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use pagesift_engine::sync::SyncRunOptions;
use pagesift_graph::{ClaimStatus, ConflictKind, NormalizedValue};
use pagesift_plan::experience::FamilyExperienceTable;
use pagesift_plan::render::family;
use pagesift_store::{FreshnessPolicy, ResourceRole};
use serde_json::json;

use super::common::{
    arc_state, run_opts, run_opts_forced, sample_spec, spawn_server, test_scraper, tick_and_get,
};

#[tokio::test]
async fn t21_embedded_json_next_data_works_without_browser_and_learns_family() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let next_html = format!(
        r#"<!doctype html><html><head><title>Next App</title></head><body>
        <div id="__next">Loading...</div>
        <script id="__NEXT_DATA__" type="application/json">{}</script>
        </body></html>"#,
        json!({
            "props": {
                "pageProps": {
                    "article": {
                        "headline": "Hydration Report 1.5.5",
                        "body": "PageSift 1.5.5 synthesizes clean structured Markdown directly from __NEXT_DATA__ payloads during HTTP Sync ticks without launching a browser instance. This significantly reduces latency and preserves fidelity across multi-tick updates while capturing SKU PROD-NEXT-155 at $199."
                    }
                }
            }
        })
    );

    let html_clone = next_html.clone();
    let app = Router::new().route(
        "/next-post",
        get(move || {
            let body = html_clone.clone();
            async move { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body) }
        }),
    );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t21", format!("{base}/next-post"));
    spec.allow_browser = false;
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    for _ in 0..2 {
        let (delta, inv) = tick_and_get(&scraper, &store, "s-t21", run_opts_forced()).await;
        assert_eq!(delta.stats.authoritative_successes, 1);
        assert_eq!(delta.stats.browser_renders_used, 0);
        assert!(inv[0].baseline_sufficient);
        assert!(
            inv[0].has_embedded_json,
            "expected embedded JSON signal learned"
        );
    }

    let exp_bytes = store
        .sync()
        .get_experience_bytes("s-t21")
        .unwrap()
        .expect("experience saved");
    let tbl = FamilyExperienceTable::new();
    assert!(tbl.decode_merge_json(&exp_bytes));
    let fam_st = tbl
        .get_family(&family(&format!("{base}/next-post")))
        .unwrap();
    assert!(
        fam_st.embedded_json.mean() > 0.55,
        "expected embedded_json posterior > 0.55, got {}",
        fam_st.embedded_json.mean()
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t22_uncertain_decode_preserves_unverified_claims_and_censors_learning() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let phase = arc_state(AtomicUsize::new(0));
    let ph = phase.clone();
    let app = Router::new().route(
        "/doc",
        get(move || {
            let p = ph.load(Ordering::SeqCst);
            async move {
                match p {
                    0 | 2 => {
                        let html =
                            super::common::product_html("Widget Pro", "SKU-DEC-1", "66", &[]);
                        let mut headers = HeaderMap::new();
                        headers.insert(header::ETAG, HeaderValue::from_static("\"v-good\""));
                        headers.insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/html; charset=utf-8"),
                        );
                        (StatusCode::OK, headers, html.into_bytes()).into_response()
                    }
                    _ => {
                        // 无 UTF-8 有效编码且声称为 utf-8 但满是不可解析字节 -> 触发 decode: 警告
                        let mut bytes = b"<html><body>Broken charset ".to_vec();
                        bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x80, 0x81, 0xFF, 0xFE, 0xFD]);
                        bytes.extend_from_slice(b"</body></html>");
                        let mut headers = HeaderMap::new();
                        headers.insert(header::ETAG, HeaderValue::from_static("\"v-bad-corrupt\""));
                        headers.insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/html; charset=utf-8"),
                        );
                        (StatusCode::OK, headers, bytes).into_response()
                    }
                }
            }
        }),
    );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t22", format!("{base}/doc"));
    spec.allow_browser = false;
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    // Tick 1: 建立健康基线
    let (d1, inv1) = tick_and_get(&scraper, &store, "s-t22", run_opts()).await;
    assert_eq!(d1.stats.authoritative_successes, 1);
    let baseline_doc_id = inv1[0].latest_doc_id.clone().expect("baseline doc");
    assert_eq!(inv1[0].etag.as_deref(), Some("\"v-good\""));

    let exp1_bytes = store.sync().get_experience_bytes("s-t22").unwrap().unwrap();
    let tbl1 = FamilyExperienceTable::new();
    tbl1.decode_merge_json(&exp1_bytes);
    let fam_key = family(&format!("{base}/doc"));
    let fam_before = tbl1.get_family(&fam_key).unwrap();

    // Tick 2: 不确定非 UTF-8 编码损坏内容
    phase.store(1, Ordering::SeqCst);
    let (d2, inv2) = tick_and_get(&scraper, &store, "s-t22", run_opts_forced()).await;
    assert_eq!(d2.stats.uncertain_observations, 1);
    assert!(inv2[0].learn.last_observation_unreliable);
    // 绝不被损坏 ETag 或最新 doc_id 覆盖基线
    assert_eq!(inv2[0].etag.as_deref(), Some("\"v-good\""));
    assert_eq!(inv2[0].latest_doc_id.as_ref(), Some(&baseline_doc_id));

    let state2 = store
        .sync()
        .build_state_view("s-t22", None)
        .unwrap()
        .unwrap();
    assert!(
        !state2.active_claims.is_empty(),
        "claims must remain preserved as Unverified, not Withdrawn"
    );
    assert!(
        state2
            .active_claims
            .iter()
            .all(|c| matches!(c.status, ClaimStatus::Unverified { .. })),
        "all prior active claims should transition to Unverified under uncertain decode"
    );

    let exp2_bytes = store.sync().get_experience_bytes("s-t22").unwrap().unwrap();
    let tbl2 = FamilyExperienceTable::new();
    tbl2.decode_merge_json(&exp2_bytes);
    let fam_after = tbl2.get_family(&fam_key).unwrap();
    assert_eq!(fam_after.http_sufficient, fam_before.http_sufficient);
    assert_eq!(fam_after.substantive_change, fam_before.substantive_change);

    // Tick 3: 恢复正常编码 -> 重新验证为 Active
    phase.store(2, Ordering::SeqCst);
    let (d3, _) = tick_and_get(&scraper, &store, "s-t22", run_opts_forced()).await;
    assert_eq!(d3.stats.uncertain_observations, 0);
    let state3 = store
        .sync()
        .build_state_view("s-t22", None)
        .unwrap()
        .unwrap();
    assert!(
        state3
            .active_claims
            .iter()
            .any(|c| c.status == ClaimStatus::Active)
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t23_hub_co_change_propagation_and_multiwave_due_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let hub_ver = arc_state(AtomicUsize::new(1));
    let hv = hub_ver.clone();
    let app = Router::new()
        .route(
            "/hub/",
            get(move || {
                let v = hv.load(Ordering::SeqCst);
                async move {
                    let title = format!("Catalog Release v{v}");
                    let html = super::common::product_html(
                        &title,
                        &format!("HUB-{v}"),
                        "10",
                        &[("/hub/item-a", "Item A"), ("/hub/item-b", "Item B")],
                    );
                    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
                }
            }),
        )
        .route(
            "/hub/item-a",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    super::common::product_html("Item A", "SKU-A", "25", &[]),
                )
            }),
        )
        .route(
            "/hub/item-b",
            get(|| async move {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    super::common::product_html("Item B", "SKU-B", "35", &[]),
                )
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t23", format!("{base}/hub/"));
    spec.freshness = FreshnessPolicy {
        min_interval_secs: 60,
        default_interval_secs: 600,
        max_interval_secs: 3600,
        confirm_gone_after_404s: 2,
    };
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    // Tick 1: Hub + 2 个同轮多波次发现的子页面全应计入 candidates_due
    let (d1, mut inv1) = tick_and_get(&scraper, &store, "s-t23", run_opts()).await;
    assert_eq!(d1.stats.requests_executed, 3);
    assert_eq!(
        d1.stats.candidates_due, 3,
        "candidates_due should accumulate multi-wave discovered items"
    );

    // 将 Hub 设成到期、而两个子页仍在遥远未来 (now + 1_200_000ms)，并更新 Hub 内容
    let now = pagesift_store::now_store_ms();
    let far_future = now + 1_200_000;
    for r in &mut inv1 {
        if r.url.ends_with("/hub/") {
            r.role = ResourceRole::Hub;
            r.next_due_at_ms = now.saturating_sub(1_000);
        } else {
            r.next_due_at_ms = far_future;
        }
    }
    store.sync().put_inventory("s-t23", &inv1, now).unwrap();
    hub_ver.store(2, Ordering::SeqCst);

    let (d2, inv2) = tick_and_get(&scraper, &store, "s-t23", run_opts()).await;
    assert!(
        d2.stats.co_changed_triggered >= 2,
        "expected hub substantive change to accelerate child next_due_at_ms: {:?}",
        d2.stats
    );
    for r in inv2.iter().filter(|r| r.url.contains("item-")) {
        assert!(
            r.next_due_at_ms < far_future,
            "child {} should be pulled forward from {far_future}, got {}",
            r.url,
            r.next_due_at_ms
        );
    }

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t24_currency_disambiguation_unknown_eur_usd_and_persisted_citation_clusters() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new()
        .route(
            "/index.html",
            get(|| async move {
                let html = r#"<!doctype html><html><body>
                <a href="/eu">EU</a>
                <a href="/unk">UNK</a>
                <a href="/us">US</a>
                <a href="/wire">Wire</a>
                <a href="/mirror1">Mirror1</a>
                <a href="/mirror2">Mirror2</a>
                </body></html>"#;
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/eu",
            get(|| async move {
                let ld = json!({
                    "@context": "https://schema.org",
                    "@type": "Product",
                    "sku": "SKU-CURR-1",
                    "offers": { "price": 66, "priceCurrency": "EUR" }
                });
                let html = format!(
                    r#"<!doctype html><html><head><script type="application/ld+json">{ld}</script></head><body><p>Product SKU-CURR-1 European store catalog offer.</p></body></html>"#
                );
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/unk",
            get(|| async move {
                // 全页没有任何货币线索的纯数值 66 -> 必须保留 UNKNOWN，不可盲标 USD
                let ld = json!({
                    "@context": "https://schema.org",
                    "@type": "Product",
                    "sku": "SKU-CURR-1",
                    "offers": { "price": 66 }
                });
                let html = format!(
                    r#"<!doctype html><html><head><script type="application/ld+json">{ld}</script></head><body><p>Product SKU-CURR-1 regional catalog without currency unit.</p></body></html>"#
                );
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/us",
            get(|| async move {
                let ld = json!({
                    "@context": "https://schema.org",
                    "@type": "Product",
                    "sku": "SKU-CURR-1",
                    "offers": { "price": "$66" }
                });
                let html = format!(
                    r#"<!doctype html><html><head><script type="application/ld+json">{ld}</script></head><body><p>Product SKU-CURR-1 US catalog priced at $66.</p></body></html>"#
                );
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/wire",
            get(|| async move {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], super::common::product_html("Wire Item", "SKU-CITE-1", "99", &[]))
            }),
        )
        .route(
            "/mirror1",
            get(|| async move {
                let mut html = super::common::product_html("Mirror One", "SKU-CITE-1", "99", &[]);
                html = html.replace(
                    "</main>",
                    "<p>Source attribution: <a href=\"/wire\">Original Wire Source</a></p></main>",
                );
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/mirror2",
            get(|| async move {
                let mut html = super::common::product_html("Mirror Two", "SKU-CITE-1", "99", &[]);
                html = html.replace(
                    "</main>",
                    "<p>Source citation: <a href=\"/wire\">Original Wire Source</a></p></main>",
                );
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    let mut spec = sample_spec("s-t24", format!("{base}/index.html"));
    spec.required_fields = Vec::new();
    spec.extract_schema = None;
    store.sync().upsert_source(spec).unwrap();

    let (delta, _inv) = tick_and_get(&scraper, &store, "s-t24", run_opts()).await;
    assert_eq!(delta.stats.requests_executed, 7);

    let state = store
        .sync()
        .build_state_view("s-t24", None)
        .unwrap()
        .unwrap();
    let curr_currencies: std::collections::BTreeSet<String> = state
        .active_claims
        .iter()
        .filter(|c| {
            c.claim.normalized_field == "price" && c.claim.subject_key.contains("SKU-CURR-1")
        })
        .filter_map(|c| match &c.claim.normalized_value {
            NormalizedValue::Money { currency, .. } => Some(currency.clone()),
            _ => None,
        })
        .collect();
    assert!(
        curr_currencies.contains("EUR")
            && curr_currencies.contains("UNKNOWN")
            && curr_currencies.contains("USD"),
        "expected distinct EUR, UNKNOWN, and USD price claims, got {curr_currencies:?}"
    );
    assert!(
        state
            .conflicts
            .iter()
            .any(|cf| cf.kind == ConflictKind::UnitOrCurrencyMismatch),
        "expected UnitOrCurrencyMismatch across EUR/UNKNOWN/USD, got {:?}",
        state.conflicts
    );

    // 二次引用独立信源统计通过持久化 StoredProvenance.citations 在 SourceStateView.fact_clusters 生效
    let cite_cluster = state
        .fact_clusters
        .iter()
        .find(|cl| cl.normalized_field == "price" && cl.subject_key.contains("SKU-CITE-1"))
        .expect("citation fact cluster present in SourceStateView");
    assert!(cite_cluster.support_count >= 3, "cluster={cite_cluster:?}");
    assert!(
        cite_cluster.independent_sources < cite_cluster.support_count,
        "secondary citations referencing /wire must collapse independent_sources ({}) below support_count ({})",
        cite_cluster.independent_sources,
        cite_cluster.support_count
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn t25_cross_source_same_tenant_transfer_and_negative_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let store = pagesift_store::Store::open_dir(dir.path()).unwrap();
    let scraper = test_scraper(Some(store.clone()));

    let app = Router::new()
        .route(
            "/catalog/v1/items/1",
            get(|| async move {
                let html = format!(
                    r#"<!doctype html><html><body><div id="__next">Loading...</div><script id="__NEXT_DATA__" type="application/json">{}</script></body></html>"#,
                    json!({
                        "props": { "pageProps": { "item": { "title": "Embedded Book", "body": "Clean book catalog item from __NEXT_DATA__ embedded state with rich description spanning sufficient length to avoid shell rejection across multiple sync ticks." } } }
                    })
                );
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        )
        .route(
            "/catalog/v1/items/new",
            get(|| async move {
                // 负迁移靶站点：虽同结构但无嵌入式 JSON 且强依赖 JS 空壳
                let html = r#"<!doctype html><html><body><div id="root">Loading...</div><script>window.__APP__=1;</script></body></html>"#;
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
            }),
        );
    let (base, shutdown) = spawn_server(app).await;

    // 1. 同一租户 alpha 下先让 source-1 成熟（学到 /catalog/v1/items/{id} 为 HTTP/EmbeddedJson 充分）
    let mut s1 = sample_spec("src-alpha-1", format!("{base}/catalog/v1/items/1"));
    s1.allow_browser = false;
    s1.required_fields = Vec::new();
    s1.extract_schema = None;
    store
        .sync_for(Some("tenant-alpha"))
        .upsert_source(s1)
        .unwrap();

    for _ in 0..4 {
        let opts = SyncRunOptions {
            tenant: Some("tenant-alpha".to_string()),
            force_all: true,
            ..Default::default()
        };
        scraper.run_sync_tick("src-alpha-1", opts).await.unwrap();
    }

    // 2. 冷启动 source-2（同租户 tenant-alpha、同结构路径），首轮将接种受控迁移，但连续遭遇空白壳矛盾后应触发负迁移拒止
    let mut s2 = sample_spec("src-alpha-2", format!("{base}/catalog/v1/items/new"));
    s2.allow_browser = false;
    s2.required_fields = Vec::new();
    s2.extract_schema = None;
    store
        .sync_for(Some("tenant-alpha"))
        .upsert_source(s2)
        .unwrap();

    for _ in 0..3 {
        let opts = SyncRunOptions {
            tenant: Some("tenant-alpha".to_string()),
            force_all: true,
            ..Default::default()
        };
        scraper.run_sync_tick("src-alpha-2", opts).await.unwrap();
    }

    let bytes_a2 = store
        .sync_for(Some("tenant-alpha"))
        .get_experience_bytes("src-alpha-2")
        .unwrap()
        .unwrap();
    let tbl_a2 = FamilyExperienceTable::new();
    tbl_a2.decode_merge_json(&bytes_a2);
    let st_a2 = tbl_a2
        .get_family(&family(&format!("{base}/catalog/v1/items/new")))
        .expect("source-2 stats");
    assert!(
        st_a2.transfer.donor_description.is_some(),
        "same-tenant mature sibling should seed initial transfer"
    );
    assert!(
        st_a2.transfer.rejected,
        "local contradictions must trigger negative-transfer rejection: {:?}",
        st_a2.transfer
    );
    assert!(st_a2.http_sufficient.mean() < 0.35);

    // 3. 不同租户 beta 完全隔离，绝不继承 alpha 的经验先验
    let mut sb = sample_spec("src-beta-1", format!("{base}/catalog/v1/items/new"));
    sb.allow_browser = false;
    sb.required_fields = Vec::new();
    sb.extract_schema = None;
    store
        .sync_for(Some("tenant-beta"))
        .upsert_source(sb)
        .unwrap();
    let opts_b = SyncRunOptions {
        tenant: Some("tenant-beta".to_string()),
        force_all: true,
        ..Default::default()
    };
    scraper.run_sync_tick("src-beta-1", opts_b).await.unwrap();
    let bytes_b = store
        .sync_for(Some("tenant-beta"))
        .get_experience_bytes("src-beta-1")
        .unwrap()
        .unwrap();
    let tbl_b = FamilyExperienceTable::new();
    tbl_b.decode_merge_json(&bytes_b);
    let st_b = tbl_b
        .get_family(&family(&format!("{base}/catalog/v1/items/new")))
        .expect("beta stats");
    assert!(
        st_b.transfer.donor_description.is_none(),
        "cross-tenant isolation must prevent alpha->beta seed"
    );

    let _ = shutdown.send(());
}
