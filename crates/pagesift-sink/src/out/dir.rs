//! 目录出口：每页一个 `.md` 文件，`index.jsonl` 每页一行。
//!
//! 有附件（`exportImages`）时写 `images/<id>`（内容寻址，同一内容只写一次），Markdown 里指向原地址或
//! 文档内部件路径的图片链接改成本地相对路径 `images/<id>`；PDF 的图片引用本来就是这个路径。

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pagesift_api::types::Document;

use super::{PageOut, page_url};

pub struct DirOut {
    dir: PathBuf,
    used: Mutex<HashSet<String>>,
}

impl DirOut {
    /// 目录不存在时创建。
    pub fn create(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            used: Mutex::new(HashSet::new()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 停止原因、覆盖度这类整次运行的汇总，和 API 任务状态里的字段一样。
    pub fn summary(&self, json: &[u8]) -> std::io::Result<()> {
        std::fs::write(self.dir.join("summary.json"), json)
    }

    fn file_name(&self, url: &str) -> String {
        let trimmed = url.split_once("://").map_or(url, |(_, rest)| rest);
        let mut name: String = trimmed
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        name.truncate(150);
        let name = name.trim_matches('_').to_string();
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        let mut candidate = format!("{name}.md");
        let mut n = 2;
        while !used.insert(candidate.clone()) {
            candidate = format!("{name}-{n}.md");
            n += 1;
        }
        candidate
    }
}

/// `index.jsonl` 的一行。
pub(super) fn index_line(doc: &Document, url: &str, file: &str) -> serde_json::Value {
    let mut line = serde_json::json!({ "url": url, "file": file, "title": doc.metadata.title });
    // 带查询的爬取：相关度和来历一起落盘，下游规则可以直接用
    if let Some(p) = doc.metadata.pagesift.as_ref() {
        if let Some(r) = p.relevance {
            line["relevance"] = serde_json::json!(r);
        }
        if let Some(t) = &p.crawl {
            line["depth"] = serde_json::json!(t.depth);
            line["parent"] = serde_json::json!(t.parent);
            line["score"] = serde_json::json!(t.score);
            if let Some(a) = &t.attachment_of {
                line["attachmentOf"] = serde_json::json!(a);
            }
        }
    }
    if !doc.attachments.is_empty() {
        line["images"] =
            serde_json::json!(doc.attachments.iter().map(|a| &a.id).collect::<Vec<_>>());
    }
    line["status"] = serde_json::json!(doc.metadata.status_code);
    // 结果有数据风险（正文薄、文件损坏……）时和 JSON 的 data.warning 一致
    if let Some(w) = &doc.warning {
        line["warning"] = serde_json::json!(w);
    }
    line
}

impl DirOut {
    /// 写附件，返回改好图片链接的 Markdown。
    fn write_images(&self, doc: &Document) -> std::io::Result<String> {
        let mut md = doc.markdown.clone().unwrap_or_default();
        if doc.attachments.is_empty() {
            return Ok(md);
        }
        let images = self.dir.join("images");
        std::fs::create_dir_all(&images)?;
        for a in &doc.attachments {
            // id 是哈希加扩展名；不是这个形状的（不该出现）不当文件名用
            if a.id.is_empty() || !a.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
                continue;
            }
            if let Some(data) = &a.data {
                let path = images.join(&a.id);
                if !path.exists() {
                    std::fs::write(&path, data)?;
                }
            }
            if let Some(src) = &a.source {
                let local = format!("images/{}", a.id);
                md = md
                    .replace(&format!("]({src})"), &format!("]({local})"))
                    .replace(&format!("]({src} \""), &format!("]({local} \""));
            }
        }
        Ok(md)
    }
}

impl PageOut for DirOut {
    fn page(&self, doc: &Document) -> std::io::Result<()> {
        let url = page_url(doc);
        let name = self.file_name(&url);
        let markdown = self.write_images(doc)?;
        std::fs::write(self.dir.join(&name), markdown)?;
        let mut index = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("index.jsonl"))?;
        writeln!(index, "{}", index_line(doc, &url, &name))?;
        eprintln!("{url} -> {name}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_api::types::Attachment;

    #[test]
    fn images_are_written_once_and_links_become_local() {
        let dir = std::env::temp_dir().join(format!("pagesift-dirout-{}", std::process::id()));
        let out = DirOut::create(&dir).unwrap();
        let web = Attachment::new(
            vec![0x89, b'P', b'N', b'G'],
            "png",
            Some("https://e.x/a.png".into()),
        );
        let pdf = Attachment::new(vec![0xFF, 0xD8, 0xFF], "jpg", None);
        let mut doc = Document {
            markdown: Some(format!(
                "![a](https://e.x/a.png)\n![b](https://e.x/a.png \"t\")\n![c](images/{})\n",
                pdf.id
            )),
            attachments: vec![web.clone(), pdf.clone()],
            ..Default::default()
        };
        doc.metadata.source_url = Some("https://e.x/page".into());
        out.page(&doc).unwrap();
        let md = std::fs::read_to_string(dir.join("e.x_page.md")).unwrap();
        assert_eq!(
            md,
            format!(
                "![a](images/{0})\n![b](images/{0} \"t\")\n![c](images/{1})\n",
                web.id, pdf.id
            )
        );
        assert_eq!(
            std::fs::read(dir.join("images").join(&web.id)).unwrap(),
            [0x89, b'P', b'N', b'G']
        );
        let index = std::fs::read_to_string(dir.join("index.jsonl")).unwrap();
        assert!(index.contains(&web.id));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
