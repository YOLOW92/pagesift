//! Single-process benchmark for PageSift DataGraph (`graph=off` vs `graph=on` A/B and streaming ingestion).
//!
//! Usage:
//!   `cargo run --release -p pagesift --example graph_bench -- ab <html_dir_or_file...>`
//!   `cargo run --release -p pagesift --example graph_bench -- stream [docs=1000]`

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use pagesift::api::types::{Format, ScrapeOptions, Upload, UploadKind};
use pagesift::{Scraper, ScraperConfig};
use pagesift_graph::{
    DocumentInput, ExtractionMethod, GraphOptions, LinkSection, RawFieldTrace, RawLinkObservation,
    RedirectKind, SourceKind, StreamingGraphBuilder, VersionRecord,
};
use serde_json::json;

#[global_allocator]
static ALLOC: pagesift_sys::SmallCache = pagesift_sys::SmallCache;

fn collect_html_files(inputs: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for raw in inputs {
        let p = Path::new(raw);
        if p.is_file() {
            files.push(p.to_path_buf());
        } else if p.is_dir()
            && let Ok(entries) = std::fs::read_dir(p)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm")
                    })
                {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    files
}

fn read_vm_hwm_kib() -> usize {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:")
            && let Some(num) = rest.split_whitespace().next()
            && let Ok(v) = num.parse::<usize>()
        {
            return v;
        }
    }
    0
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64) * p).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

async fn run_ab(inputs: &[String]) {
    let paths = collect_html_files(inputs);
    assert!(!paths.is_empty(), "no HTML inputs found");
    let payloads: Vec<(String, Vec<u8>)> = paths
        .into_iter()
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("page.html")
                .to_string();
            let bytes = std::fs::read(&p).expect("read html");
            (name, bytes)
        })
        .collect();

    let mut cfg = ScraperConfig::default();
    cfg.fetcher.ssrf.allow_private_network = true;
    let scraper = Scraper::new(cfg).expect("scraper");

    let off_opts = ScrapeOptions {
        formats: vec![Format::new("markdown")],
        only_main_content: true,
        ..Default::default()
    };
    let on_opts = ScrapeOptions {
        formats: vec![Format::new("markdown"), Format::new("graph")],
        only_main_content: true,
        graph: Some(GraphOptions::enabled()),
        ..Default::default()
    };

    // Warm up allocator, regexes, and HTML tables on the first 8 files.
    for (name, bytes) in payloads.iter().take(8) {
        let up = Upload {
            bytes: bytes.clone(),
            filename: name.clone(),
            content_type: Some("text/html".into()),
            kind: UploadKind::Html,
            url: url::Url::parse(&format!("https://bench.local/{name}")).ok(),
        };
        let _ = scraper.parse(up.clone(), &off_opts).await;
        let _ = scraper.parse(up, &on_opts).await;
    }

    let mut off_ms = Vec::with_capacity(payloads.len());
    let off_wall_t0 = Instant::now();
    for (name, bytes) in &payloads {
        let up = Upload {
            bytes: bytes.clone(),
            filename: name.clone(),
            content_type: Some("text/html".into()),
            kind: UploadKind::Html,
            url: url::Url::parse(&format!("https://bench.local/{name}")).ok(),
        };
        let t0 = Instant::now();
        let doc = scraper.parse(up, &off_opts).await.expect("parse off");
        off_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        black_box(&doc);
    }
    let off_wall_s = off_wall_t0.elapsed().as_secs_f64();
    let off_rss_kib = read_vm_hwm_kib();

    let mut on_ms = Vec::with_capacity(payloads.len());
    let mut delta_bytes = 0usize;
    let mut blocks = 0usize;
    let mut citations = 0usize;
    let mut provenance = 0usize;
    let mut evidence = 0usize;
    let mut claims = 0usize;
    let on_wall_t0 = Instant::now();
    for (name, bytes) in &payloads {
        let up = Upload {
            bytes: bytes.clone(),
            filename: name.clone(),
            content_type: Some("text/html".into()),
            kind: UploadKind::Html,
            url: url::Url::parse(&format!("https://bench.local/{name}")).ok(),
        };
        let t0 = Instant::now();
        let doc = scraper.parse(up, &on_opts).await.expect("parse on");
        on_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        if let Some(ref g) = doc.graph {
            blocks += g.blocks.len();
            citations += g.citations.len();
            provenance += g.provenance.len();
            evidence += g.evidence.len();
            claims += g.claims.len();
            delta_bytes += serde_json::to_vec(g).map_or(0, |v| v.len());
        }
        black_box(&doc);
    }
    let on_wall_s = on_wall_t0.elapsed().as_secs_f64();
    let on_rss_kib = read_vm_hwm_kib();

    off_ms.sort_by(f64::total_cmp);
    on_ms.sort_by(f64::total_cmp);
    let n = payloads.len() as f64;
    let off_tput = n / off_wall_s;
    let on_tput = n / on_wall_s;
    let off_p50 = pct(&off_ms, 0.50);
    let off_p95 = pct(&off_ms, 0.95);
    let on_p50 = pct(&on_ms, 0.50);
    let on_p95 = pct(&on_ms, 0.95);
    let p50_overhead = if off_p50 > 0.0 {
        ((on_p50 - off_p50) / off_p50) * 100.0
    } else {
        0.0
    };

    let summary = json!({
        "pages": payloads.len(),
        "off": {
            "p50_ms": off_p50,
            "p95_ms": off_p95,
            "wall_s": off_wall_s,
            "pages_s": off_tput,
            "vm_hwm_mb": (off_rss_kib as f64) / 1024.0,
        },
        "on": {
            "p50_ms": on_p50,
            "p95_ms": on_p95,
            "wall_s": on_wall_s,
            "pages_s": on_tput,
            "vm_hwm_mb": (on_rss_kib as f64) / 1024.0,
            "delta_bytes": delta_bytes,
            "blocks": blocks,
            "citations": citations,
            "provenance": provenance,
            "evidence": evidence,
            "claims": claims,
        },
        "p50_overhead_pct": p50_overhead,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

fn run_stream(docs: usize) {
    let inputs: Vec<DocumentInput> = (0..docs)
        .map(|i| {
            let group = i / 4;
            let version_idx = i % 2;
            let conflict_bump = i % 11 == 0;
            let price = if conflict_bump { 129.0 } else { 99.0 };
            let md = format!(
                "# Widget Edition {group}\n\n\
                 Enterprise plan priced at ${price:.2}/month for ACME-SKU-{group:04}.\n\n\
                 Updated specification with [datasheet PDF](https://docs.example.com/specs/{group}.pdf) and [reference](https://ref.example.com/item/{}).\n",
                (group + 1) % (docs.max(4) / 4)
            );
            let source_kind = match i % 4 {
                0 => SourceKind::Html,
                1 => SourceKind::Pdf,
                2 => SourceKind::Office {
                    format: "docx".into(),
                },
                _ => SourceKind::Api,
            };
            DocumentInput {
                requested_url: format!("https://shop.example.com/redirect/{i}"),
                final_url: format!("https://shop.example.com/items/{group}?src={i}"),
                canonical_url: Some(format!("https://shop.example.com/items/{group}")),
                canonical_from_header: i % 3 == 0,
                source_kind,
                observed_at_ms: 1_760_000_000_000 + (version_idx as u64) * 3_600_000 + (i as u64),
                status_code: 200,
                engine: "http".into(),
                content_hash: Some(format!("ch_{group}_{version_idx}_{conflict_bump}")),
                raw_hash: Some(format!("rh_{i}")),
                text_hash: Some(format!("th_{group}_{version_idx}_{conflict_bump}")),
                etag: Some(format!("\"etag-{group}-{version_idx}\"")),
                last_modified: Some("Wed, 12 Nov 2026 00:00:00 GMT".into()),
                title: Some(format!("Widget Edition {group}")),
                markdown: md,
                redirect_chain: vec![(
                    format!("https://shop.example.com/redirect/{i}"),
                    format!("https://shop.example.com/items/{group}?src={i}"),
                    301,
                    RedirectKind::HttpPermanent,
                )],
                links: vec![
                    RawLinkObservation {
                        target_url: format!("https://docs.example.com/specs/{group}.pdf"),
                        anchor_text: "datasheet PDF".into(),
                        rel: Some("enclosure".into()),
                        section: LinkSection::MainContent,
                        heading_path: vec![format!("Widget Edition {group}")],
                        block_id: Some("b_2".into()),
                    },
                    RawLinkObservation {
                        target_url: format!(
                            "https://ref.example.com/item/{}",
                            (group + 1) % (docs.max(4) / 4)
                        ),
                        anchor_text: "reference".into(),
                        rel: None,
                        section: LinkSection::MainContent,
                        heading_path: vec![format!("Widget Edition {group}")],
                        block_id: Some("b_2".into()),
                    },
                ],
                alternates: Vec::new(),
                duplicate_of: None,
                attachments: vec![(
                    format!("https://docs.example.com/specs/{group}.pdf"),
                    None,
                    None,
                )],
                structured_json_ld: vec![json!({
                    "@type": "Product",
                    "name": format!("Widget Edition {group}"),
                    "sku": format!("ACME-SKU-{group:04}"),
                    "price": format!("${price:.2}/month")
                })],
                extracted_fields: Some(json!({
                    "price": format!("${price:.2}/month"),
                    "sku": format!("ACME-SKU-{group:04}")
                })),
                raw_traces: vec![RawFieldTrace {
                    field_path: "price".into(),
                    field_name: "price".into(),
                    value: json!(format!("${price:.2}/month")),
                    raw_quote: format!("${price:.2}/month"),
                    attr_name: None,
                    method: ExtractionMethod::CssSelector {
                        selector: ".price".into(),
                        relocated_from: None,
                        step: None,
                    },
                    source_url: Some(format!("https://shop.example.com/items/{group}")),
                    visible_in_dom: true,
                }],
                chunk_spans: Vec::new(),
                previous_version: (version_idx == 1).then(|| VersionRecord {
                    doc_id: format!("prev_{group}"),
                    resource_key: format!("https://shop.example.com/items/{group}"),
                    url: format!("https://shop.example.com/items/{group}"),
                    canonical_url: Some(format!("https://shop.example.com/items/{group}")),
                    status_code: 200,
                    observed_at_ms: 1_760_000_000_000,
                    content_hash: Some(format!("ch_{group}_0_false")),
                    text_hash: Some(format!("th_{group}_0_false")),
                    etag: Some(format!("\"etag-{group}-0\"")),
                    last_modified: Some("Tue, 11 Nov 2026 00:00:00 GMT".into()),
                }),
                version_diff_stats: None,
                changed_block_ids: Vec::new(),
            }
        })
        .collect();

    let mut builder = StreamingGraphBuilder::new(GraphOptions::enabled());
    let mut delta_bytes = 0usize;
    let t_ingest = Instant::now();
    for doc in inputs {
        let delta = builder.ingest(doc);
        delta_bytes += serde_json::to_vec(&delta).map_or(0, |v| v.len());
    }
    let ingest_ms = t_ingest.elapsed().as_secs_f64() * 1e3;

    let t_fin = Instant::now();
    let summary = builder.finalize();
    let finalize_ms = t_fin.elapsed().as_secs_f64() * 1e3;
    let summary_bytes = serde_json::to_vec(&summary).map_or(0, |v| v.len());
    let total_ms = ingest_ms + finalize_ms;
    let rss_mb = (read_vm_hwm_kib() as f64) / 1024.0;

    let out = BTreeMap::from([
        ("docs", json!(docs)),
        ("ingest_ms", json!(ingest_ms)),
        ("finalize_ms", json!(finalize_ms)),
        ("total_ms", json!(total_ms)),
        ("docs_s", json!((docs as f64) / (total_ms / 1e3))),
        ("delta_bytes", json!(delta_bytes)),
        ("summary_bytes", json!(summary_bytes)),
        ("vm_hwm_mb", json!(rss_mb)),
        ("stats", json!(summary.stats)),
    ]);
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("ab") => {
            let inputs: Vec<String> = args.collect();
            run_ab(&inputs).await;
        }
        Some("stream") => {
            let docs: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);
            run_stream(docs);
        }
        _ => {
            eprintln!("usage: graph_bench ab <html_dir...> | stream [docs]");
            std::process::exit(2);
        }
    }
}
