//! Stress testing for ISO 28500 WARC/1.1 writer, multi-member Gzip, and cryptographic integrity.

use flate2::read::MultiGzDecoder;
use pagesift_store::warc::{DigestCalculator, RecordType, WarcRecord, WarcWriter, WebpageSnapshot};
use std::io::{Cursor, Read};
use std::time::Instant;

#[test]
fn test_stress_warc_throughput_and_multi_member_decompression() {
    let num_pages = 500;
    let mut gz_buffer = Vec::new();

    // Prepare realistic multi-resource page snapshots (~16 KB per page)
    let sample_html = r#"<!DOCTYPE html>
<html>
<head>
    <title>High Throughput Stress Page</title>
    <link rel="stylesheet" href="/static/css/main.css">
    <script src="/static/js/bundle.js"></script>
    <style>@font-face { src: url('/static/fonts/inter.woff2'); }</style>
</head>
<body>
    <h1>Article Benchmark</h1>
    <p>"#
        .to_string()
        + &"Content paragraph text for archiving testing and web indexing. ".repeat(250)
        + r#"</p>
    <img src="/static/img/hero.png" alt="hero">
</body>
</html>"#;

    let sample_css = b"body { font-family: Inter, sans-serif; } .hero { width: 100%; }".repeat(20);
    let sample_js =
        b"console.log('Production app initialized'); function run() { return 42; }".repeat(30);
    let sample_png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]; // PNG signature

    let mut snapshots = Vec::with_capacity(num_pages);
    for i in 0..num_pages {
        snapshots.push(WebpageSnapshot {
            target_uri: format!("https://news.example.com/articles/item_{}", i),
            status_code: 200,
            html_content: sample_html.as_bytes().to_vec(),
            http_headers: vec![
                ("Content-Type".into(), "text/html; charset=utf-8".into()),
                ("Server".into(), "nginx".into()),
            ],
            assets: vec![
                pagesift_store::warc::SubresourceAsset {
                    uri: format!("https://news.example.com/static/css/main_{}.css", i),
                    mime_type: "text/css".into(),
                    content: sample_css.clone(),
                },
                pagesift_store::warc::SubresourceAsset {
                    uri: format!("https://news.example.com/static/js/bundle_{}.js", i),
                    mime_type: "application/javascript".into(),
                    content: sample_js.clone(),
                },
                pagesift_store::warc::SubresourceAsset {
                    uri: format!("https://news.example.com/static/img/hero_{}.png", i),
                    mime_type: "image/png".into(),
                    content: sample_png.clone(),
                },
            ],
        });
    }

    let start = Instant::now();
    let mut total_uncompressed_bytes = 0usize;
    let mut total_records = 0usize;

    {
        let mut writer = WarcWriter::new(&mut gz_buffer, true);
        let winfo_id = writer
            .write_warcinfo("stress_archive.warc.gz", "PageSift 0.40")
            .unwrap();
        total_records += 1;

        for snap in &snapshots {
            // 1. Root response record
            let mut root_record = WarcRecord::new(
                RecordType::Response,
                "application/http; msgtype=response",
                snap.html_content.clone(),
            )
            .with_target_uri(&snap.target_uri)
            .with_http_response(snap.status_code, snap.http_headers.clone());
            root_record.warcinfo_id = Some(winfo_id.clone());

            total_uncompressed_bytes += snap.html_content.len();
            total_records += 1;

            // 2. Secondary asset records linked via WARC-Concurrent-To
            let root_id = root_record.record_id.clone();
            writer.write_record(&root_record).unwrap();

            for asset in &snap.assets {
                let mut asset_record = WarcRecord::new(
                    RecordType::Resource,
                    &asset.mime_type,
                    asset.content.clone(),
                )
                .with_target_uri(&asset.uri);
                asset_record.add_concurrent_to(&root_id);
                asset_record.warcinfo_id = Some(winfo_id.clone());

                total_uncompressed_bytes += asset.content.len();
                total_records += 1;
                writer.write_record(&asset_record).unwrap();
            }
        }
        writer.flush().unwrap();
    }

    let elapsed = start.elapsed();
    let throughput_mbs =
        (total_uncompressed_bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();
    let records_per_sec = total_records as f64 / elapsed.as_secs_f64();

    println!(
        "\n[STRESS WARC] Wrote {} records ({} uncompressed MB) in {:.3} s",
        total_records,
        total_uncompressed_bytes / (1024 * 1024),
        elapsed.as_secs_f64()
    );
    println!(
        "[STRESS WARC] Pure Rust Throughput: {:.2} MB/s ({:.1} records/s)",
        throughput_mbs, records_per_sec
    );
    println!(
        "[STRESS WARC] Compressed WARC Size: {:.2} MB",
        gz_buffer.len() as f64 / (1024.0 * 1024.0)
    );

    // Verify throughput is strictly superior to warcio baseline
    let min_expected = if cfg!(debug_assertions) { 5.0 } else { 50.0 };
    assert!(
        throughput_mbs > min_expected,
        "Throughput {:.2} MB/s must beat baseline threshold ({:.1} MB/s)",
        throughput_mbs,
        min_expected
    );

    // Decompress with multi-member gzip decoder and verify integrity
    let mut decoder = MultiGzDecoder::new(Cursor::new(&gz_buffer));
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).unwrap();

    let decomp_str = String::from_utf8_lossy(&decompressed);
    assert!(decomp_str.contains("WARC/1.1\r\n"));
    assert!(decomp_str.contains("WARC-Type: warcinfo\r\n"));
    assert!(decomp_str.contains("WARC-Type: response\r\n"));
    assert!(decomp_str.contains("WARC-Type: resource\r\n"));
    assert!(decomp_str.contains("WARC-Concurrent-To: <urn:uuid:"));
}

#[test]
fn test_stress_tamper_detection_on_warc_records() {
    let payload = b"Original pristine webpage content before tamper".to_vec();
    let digests = DigestCalculator::digest_bytes(&payload);

    let mut tampered = payload.clone();
    tampered[10] ^= 0xFF; // Flip bits
    let tampered_digests = DigestCalculator::digest_bytes(&tampered);

    assert_ne!(digests.sha256_hex, tampered_digests.sha256_hex);
    assert_ne!(digests.blake3_hex, tampered_digests.blake3_hex);
}
