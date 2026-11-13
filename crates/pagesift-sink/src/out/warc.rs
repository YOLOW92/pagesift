//! WARC 出口（ISO 28500 WARC/1.1，写入器在 `pagesift_store::warc`）。
//!
//! 每页三条记录，后两条用 `WARC-Concurrent-To` 指向第一条：
//! - `resource`：页面 HTML。pagesift 不保留网络上的原始字节和响应头，这里是解码成 UTF-8 后的 HTML，
//!   所以用 `resource` 而不是 `response`，`Content-Type` 写 `text/html; charset=utf-8`，不冒充原始响应；
//! - `conversion`：提取出的 Markdown；
//! - `metadata`：Firecrawl 的 `metadata` 对象（JSON）。
//!
//! 文件名以 `.gz` 结尾时按每条记录一个 gzip 成员写（WARC 规范附录 D 的做法）。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Mutex;

use pagesift_api::types::Document;
use pagesift_graph::GraphSummary;
use pagesift_store::warc::{RecordType, WarcRecord, WarcWriter};

use super::{PageOut, page_url};

pub struct WarcOut {
    writer: Mutex<WarcWriter<BufWriter<File>>>,
}

impl WarcOut {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let gzip = path.extension().is_some_and(|e| e == "gz");
        let mut writer = WarcWriter::new(BufWriter::new(File::create(path)?), gzip);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        writer.write_warcinfo(
            name.as_deref().unwrap_or_default(),
            concat!("pagesift ", env!("CARGO_PKG_VERSION")),
        )?;
        Ok(Self {
            writer: Mutex::new(writer),
        })
    }
}

impl PageOut for WarcOut {
    fn page(&self, doc: &Document) -> std::io::Result<()> {
        let url = page_url(doc);
        let html = doc
            .raw_html
            .as_deref()
            .or(doc.html.as_deref())
            .unwrap_or_default();
        let head = WarcRecord::new(
            RecordType::Resource,
            "text/html; charset=utf-8",
            html.as_bytes().to_vec(),
        )
        .with_target_uri(&url);
        let mut records = vec![];
        if let Some(md) = &doc.markdown {
            records.push(WarcRecord::new(
                RecordType::Conversion,
                "text/markdown; charset=utf-8",
                md.as_bytes().to_vec(),
            ));
        }
        records.push(WarcRecord::new(
            RecordType::Metadata,
            "application/json",
            serde_json::to_vec(&doc.metadata).map_err(std::io::Error::other)?,
        ));
        if let Some(slice) = &doc.graph {
            records.push(WarcRecord::new(
                RecordType::Metadata,
                "application/vnd.pagesift.graph+json",
                serde_json::to_vec(slice).map_err(std::io::Error::other)?,
            ));
        }
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_record(&head)?;
        for mut r in records {
            r.target_uri = Some(url.clone());
            r.add_concurrent_to(&head.record_id);
            w.write_record(&r)?;
        }
        Ok(())
    }

    fn graph_summary(&self, summary: &GraphSummary) -> std::io::Result<()> {
        let payload = serde_json::to_vec(summary).map_err(std::io::Error::other)?;
        let rec = WarcRecord::new(
            RecordType::Metadata,
            "application/vnd.pagesift.graph-summary+json",
            payload,
        );
        self.writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_record(&rec)
    }

    fn finish(&self) -> std::io::Result<()> {
        self.writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_api::types::Metadata;

    #[test]
    fn writes_resource_conversion_and_metadata_per_page() {
        let dir = std::env::temp_dir().join(format!("pagesift-warc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.warc");
        let out = WarcOut::create(&path).unwrap();
        let doc = Document {
            markdown: Some("# Hi".into()),
            raw_html: Some("<h1>Hi</h1>".into()),
            metadata: Metadata {
                source_url: Some("https://e.com/".into()),
                status_code: 200,
                ..Default::default()
            },
            ..Default::default()
        };
        out.page(&doc).unwrap();
        out.finish().unwrap();
        let text = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        for t in ["warcinfo", "resource", "conversion", "metadata"] {
            assert!(text.contains(&format!("WARC-Type: {t}")), "{t}");
        }
        assert_eq!(text.matches("WARC-Concurrent-To").count(), 2);
        assert!(text.contains("<h1>Hi</h1>") && text.contains("# Hi"));
    }

    #[test]
    fn writes_optional_graph_slice_and_summary_records() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("graph.warc");
        let out = WarcOut::create(&path).unwrap();
        let mut builder =
            pagesift_graph::StreamingGraphBuilder::new(pagesift_graph::GraphOptions::enabled());
        let delta = builder.ingest(pagesift_graph::DocumentInput {
            requested_url: "https://e.com/".into(),
            final_url: "https://e.com/".into(),
            canonical_url: None,
            canonical_from_header: false,
            source_kind: pagesift_graph::SourceKind::Html,
            observed_at_ms: 100,
            status_code: 200,
            engine: "http".into(),
            content_hash: Some("h1".into()),
            raw_hash: None,
            text_hash: None,
            etag: None,
            last_modified: None,
            title: Some("Hi".into()),
            markdown: "# Hi".into(),
            redirect_chain: Vec::new(),
            links: Vec::new(),
            alternates: Vec::new(),
            duplicate_of: None,
            attachments: Vec::new(),
            structured_json_ld: Vec::new(),
            extracted_fields: None,
            raw_traces: Vec::new(),
            chunk_spans: Vec::new(),
            previous_version: None,
            version_diff_stats: None,
            changed_block_ids: Vec::new(),
        });
        let summary = builder.finalize();
        let doc = Document {
            markdown: Some("# Hi".into()),
            raw_html: Some("<h1>Hi</h1>".into()),
            graph: Some(pagesift_graph::GraphSlice::from(delta)),
            metadata: Metadata {
                source_url: Some("https://e.com/".into()),
                status_code: 200,
                ..Default::default()
            },
            ..Default::default()
        };
        out.page(&doc).unwrap();
        out.graph_summary(&summary).unwrap();
        out.finish().unwrap();

        let text = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(text.matches("WARC-Concurrent-To").count(), 3);
        assert!(text.contains("Content-Type: application/vnd.pagesift.graph+json"));
        assert!(text.contains("Content-Type: application/vnd.pagesift.graph-summary+json"));
    }
}
