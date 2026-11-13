//! 命令行的页面出口：scrape、crawl、convert 共用同一种布局。
//!
//! - 目录（`--out`）：每页一个 `.md`，加一行 `index.jsonl`；crawl 结束时另写 `summary.json`。
//! - 标准输出：每页一行 JSON（`url`、`title`、`markdown`、`warning`）。
//! - WARC（`--warc`）：每页一条 `resource`（HTML）、一条 `conversion`（Markdown）、一条 `metadata`（元数据 JSON）。
//!
//! 一个命令可以同时挂多个出口（[`Fanout`]）。

mod dir;
mod lines;
mod warc;

pub use dir::DirOut;
pub use lines::LinesOut;
pub use warc::WarcOut;

use pagesift_api::types::Document;
use pagesift_graph::{GraphSummary, PageGraphDelta};

/// 一个页面出口。
pub trait PageOut: Send + Sync {
    fn page(&self, doc: &Document) -> std::io::Result<()>;

    /// Stream one per-page DataGraph delta (optional for outputs that support it).
    fn graph_delta(&self, _delta: &PageGraphDelta) -> std::io::Result<()> {
        Ok(())
    }

    /// Persist the final job-level DataGraph summary (optional for outputs that support it).
    fn graph_summary(&self, _summary: &GraphSummary) -> std::io::Result<()> {
        Ok(())
    }

    /// 所有页面写完时调用（落盘缓冲）。
    fn finish(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 同时写多个出口；每个出口都写一遍，返回遇到的第一个错误。
#[derive(Default)]
pub struct Fanout(pub Vec<Box<dyn PageOut>>);

impl PageOut for Fanout {
    fn page(&self, doc: &Document) -> std::io::Result<()> {
        each(&self.0, |o| o.page(doc))
    }

    fn graph_delta(&self, delta: &PageGraphDelta) -> std::io::Result<()> {
        each(&self.0, |o| o.graph_delta(delta))
    }

    fn graph_summary(&self, summary: &GraphSummary) -> std::io::Result<()> {
        each(&self.0, |o| o.graph_summary(summary))
    }

    fn finish(&self) -> std::io::Result<()> {
        each(&self.0, |o| o.finish())
    }
}

fn each(
    outs: &[Box<dyn PageOut>],
    f: impl Fn(&dyn PageOut) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut first = Ok(());
    for o in outs {
        if let Err(e) = f(o.as_ref()) {
            first = first.and(Err(e));
        }
    }
    first
}

/// 页面的地址：请求的地址优先，没有时用最终地址。
pub fn page_url(doc: &Document) -> String {
    doc.metadata
        .source_url
        .clone()
        .or_else(|| doc.metadata.url.clone())
        .unwrap_or_default()
}
