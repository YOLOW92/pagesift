//! Pure Safe Rust ISO 28500 WARC/1.1 Archive Engine with Multi-Member Gzip and Dual Hashes.

pub mod asset_snapshot;
pub mod checksum;
pub mod warc_writer;

pub use asset_snapshot::{AssetSniffer, SubresourceAsset, WebpageSnapshot};
pub use checksum::{DigestCalculator, WarcDigests};
pub use warc_writer::{RecordType, WarcRecord, WarcWriter};

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::MultiGzDecoder;
    use std::io::Read;

    #[test]
    fn test_warc_writer_iso28500_compliance() {
        let mut buffer = Vec::new();
        {
            let mut writer = WarcWriter::new(&mut buffer, false);
            let winfo_id = writer
                .write_warcinfo("test_archive.warc", "PageSift 0.40")
                .unwrap();
            assert!(winfo_id.starts_with("urn:uuid:"));

            let html_payload = b"<!DOCTYPE html><html><body><h1>Hello WARC</h1></body></html>".to_vec();
            let mut record = WarcRecord::new(RecordType::Response, "application/http; msgtype=response", html_payload)
                .with_target_uri("https://example.com/index.html")
                .with_http_response(200, vec![
                    ("Content-Type".into(), "text/html; charset=utf-8".into()),
                    ("Server".into(), "PageSift-Test".into()),
                ]);
            record.add_concurrent_to("urn:uuid:12345678-1234-1234-1234-123456789abc");
            writer.write_record(&record).unwrap();
        }

        let output_str = String::from_utf8_lossy(&buffer);
        assert!(output_str.starts_with("WARC/1.1\r\n"));
        assert!(output_str.contains("WARC-Type: warcinfo\r\n"));
        assert!(output_str.contains("WARC-Type: response\r\n"));
        assert!(output_str.contains("WARC-Target-URI: https://example.com/index.html\r\n"));
        assert!(output_str.contains("WARC-Concurrent-To: <urn:uuid:12345678-1234-1234-1234-123456789abc>\r\n"));
        assert!(output_str.contains("WARC-Block-Digest: sha256:"));
        assert!(output_str.contains("WARC-Payload-Digest: sha256:"));
        assert!(output_str.contains("HTTP/1.1 200 OK\r\n"));
        assert!(output_str.contains("Server: PageSift-Test\r\n"));
        assert!(output_str.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_warc_multi_member_gzip_decompression() {
        let mut gz_buffer = Vec::new();
        {
            let mut writer = WarcWriter::new(&mut gz_buffer, true);
            writer.write_warcinfo("compressed.warc.gz", "PageSift 0.40").unwrap();

            let payload = b"Resource payload content 12345".to_vec();
            let record = WarcRecord::new(RecordType::Resource, "text/plain", payload)
                .with_target_uri("https://example.com/asset.txt");
            writer.write_record(&record).unwrap();
        }

        // Verify standard MultiGzDecoder can read both independent gzip members
        let mut decoder = MultiGzDecoder::new(&gz_buffer[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).unwrap();

        assert!(decompressed.contains("WARC-Type: warcinfo\r\n"));
        assert!(decompressed.contains("WARC-Type: resource\r\n"));
        assert!(decompressed.contains("https://example.com/asset.txt"));
        assert!(decompressed.contains("Resource payload content 12345"));
    }

    #[test]
    fn test_asset_sniffer_extraction() {
        let html = r#"<!DOCTYPE html>
<html>
<head>
  <link rel="stylesheet" href="/assets/style.css">
  <link rel="icon" href="/favicon.ico">
  <script src="https://cdn.example.com/app.js"></script>
  <style>
    @font-face { src: url('/fonts/custom.woff2'); }
  </style>
</head>
<body>
  <img src="banner.png" alt="banner">
  <a href="/about.html">About Page</a>
</body>
</html>"#;
        let urls = AssetSniffer::sniff_asset_urls(html);
        assert!(urls.contains(&"/assets/style.css".to_string()));
        assert!(urls.contains(&"/favicon.ico".to_string()));
        assert!(urls.contains(&"https://cdn.example.com/app.js".to_string()));
        assert!(urls.contains(&"/fonts/custom.woff2".to_string()));
        assert!(urls.contains(&"banner.png".to_string()));
        // Pure navigation link should not be classified as static asset
        assert!(!urls.contains(&"/about.html".to_string()));
    }
}
