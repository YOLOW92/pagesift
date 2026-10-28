//! 附件收割：带查询的爬取里，相关页面上链到的 PDF、Office 文档单独排队、单独计预算。
//!
//! 政策、学费表、申请指南常常只以 PDF 挂在页面上。不开这个选项时，文档链接和普通页面一样
//! 受路径范围约束、占页面预算；开了以后，相关度过线的页面上同一站点的文档不受路径范围限制，
//! 按链接分数排队，用自己的预算（`attachmentLimit`，默认等于 `limit`），抓回来走现有的
//! PDF/Office 解析管线，结果里 `metadata.pagesift.crawl.attachmentOf` 记着来源页。

use std::collections::HashSet;

use pagesift_net::Url;

use crate::focus::{BestFirst, Offer};

/// 当作附件的文件扩展名（小写）。
const DOCUMENT_EXT: &[&str] = &[
    "pdf", "docx", "docm", "dotx", "xlsx", "xlsm", "xltx", "pptx", "pptm", "ppsx", "epub",
];

/// URL 的路径以文档扩展名结尾。
pub(crate) fn is_document_url(u: &Url) -> bool {
    let last = u.path().rsplit('/').next().unwrap_or_default();
    last.rsplit_once('.')
        .is_some_and(|(_, ext)| DOCUMENT_EXT.contains(&ext.to_ascii_lowercase().as_str()))
}

/// 附件队列：有界，按分数出队。
pub(crate) struct Attachments {
    queue: BestFirst,
    seen: HashSet<u64>,
    pub(crate) limit: usize,
    pub(crate) scheduled: usize,
}

impl Attachments {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            queue: BestFirst::default(),
            seen: HashSet::new(),
            limit,
            scheduled: 0,
        }
    }

    pub(crate) fn pending(&self) -> usize {
        self.queue.len()
    }

    /// 放入一个附件链接；已经见过的忽略。
    pub(crate) fn offer(&mut self, hash: u64, url: Url, depth: usize, score: f32) {
        if self.seen.contains(&hash) {
            return;
        }
        let room = self.limit.saturating_sub(self.scheduled);
        match self.queue.offer(hash, url, depth, score, (), room) {
            Offer::Queued => {
                self.seen.insert(hash);
            }
            Offer::Evicted(old) => {
                self.seen.remove(&old);
                self.seen.insert(hash);
            }
            Offer::Rejected | Offer::AlreadyQueued => {}
        }
    }

    /// 下一个要抓的附件：(URL, 深度, 分数)。预算用完时 None。
    pub(crate) fn next(&mut self) -> Option<(Url, usize, f32)> {
        if self.scheduled >= self.limit {
            return None;
        }
        let item = self
            .queue
            .pop_diverse(&Default::default(), usize::MAX, |_| false)?;
        self.scheduled += 1;
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn document_urls_are_recognised_by_extension() {
        for s in [
            "https://e.edu/files/fees.pdf",
            "https://e.edu/x/guide.PDF",
            "https://e.edu/forms/apply.docx",
            "https://e.edu/data/tuition.xlsx?year=2026",
            "https://e.edu/book.epub",
        ] {
            assert!(is_document_url(&u(s)), "{s}");
        }
        for s in [
            "https://e.edu/pdf/",
            "https://e.edu/files/fees.pdf.html",
            "https://e.edu/image.png",
            "https://e.edu/",
        ] {
            assert!(!is_document_url(&u(s)), "{s}");
        }
    }

    #[test]
    fn attachment_budget_is_bounded_and_ordered() {
        let mut a = Attachments::new(2);
        a.offer(1, u("https://e.edu/a.pdf"), 1, 0.2);
        a.offer(2, u("https://e.edu/b.pdf"), 1, 0.9);
        a.offer(2, u("https://e.edu/b.pdf"), 1, 0.9);
        a.offer(3, u("https://e.edu/c.pdf"), 1, 0.5);
        assert_eq!(a.pending(), 2, "the lowest is evicted");
        assert_eq!(a.next().unwrap().0.path(), "/b.pdf");
        assert_eq!(a.next().unwrap().0.path(), "/c.pdf");
        assert!(a.next().is_none(), "budget spent");
    }
}
