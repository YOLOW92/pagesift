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
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_record(&head)?;
        for mut r in records {
            r.target_uri = Some(url.clone());
            r.add_concurrent_to(&head.record_id);
            w.write_record(&r)?;
        }
        Ok(())
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
}
