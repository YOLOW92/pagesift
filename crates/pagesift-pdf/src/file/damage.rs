//! 文件损坏的记录：xref 读不出靠扫描重建、偏移指错靠扫描纠正、页树里引用的页面对象丢了。
//!
//! 修复本身是静默的（能恢复多少就给多少），但调用方得知道结果可能不完整：截断的 PDF 恢复出前几页
//! 时，和网页正文不完整一样要有警告，不能让少了的页悄悄消失。

/// 页树遍历的结果：找到的页面，以及文件自己声明的页数和丢失的子节点数。
#[derive(Debug, Default)]
pub(crate) struct PageTree {
    pub(crate) ids: Vec<super::ObjectId>,
    /// 根 /Pages 字典的 /Count（写入方声明的页数）；没有或不是整数时为 None。
    pub(crate) declared: Option<usize>,
    /// 页树里引用了、但对象不存在、只剩半截或不是页面字典的子节点数（截断、损坏的文件）。
    pub(crate) missing: usize,
    /// 页树根丢了，页面是扫描散落的页面对象找回的，文件里也没有任何地方还写着总页数：
    /// 后面的页可能丢了，但丢了几页不知道。
    pub(crate) count_unknown: bool,
}

/// 丢了内容流的一页：第几页（从 1 数），这一页一共几段内容流，丢了几段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LostContent {
    pub(crate) page: usize,
    pub(crate) lost: usize,
    pub(crate) streams: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Damage {
    /// xref 不可用，对象索引是扫描全文重建的。
    pub(crate) repaired: bool,
    /// xref 可用但有偏移指错的对象，靠扫描全文找到。
    pub(crate) rescued: bool,
    pub(crate) declared_pages: Option<usize>,
    pub(crate) missing_pages: usize,
    /// 见 [`PageTree::count_unknown`]。
    pub(crate) page_count_unknown: bool,
    /// 页面在、但内容流丢了（全部或其中几段）的页：页数对，正文不全。
    pub(crate) incomplete_pages: Vec<LostContent>,
    /// 内容流在、但字体字典丢了的页：字还在，可能解码错。
    pub(crate) fontless_pages: Vec<usize>,
}

/// 警告里最多列出这么多个页码。
const LISTED_PAGES: usize = 10;

impl Damage {
    /// 给调用方的警告，格式和引擎其他警告一样：`标识: 说明`。`found` 是实际找到的页数。
    pub(crate) fn warnings(&self, found: usize) -> Vec<String> {
        let mut out = Vec::new();
        if self.repaired {
            out.push(
                "pdf_repaired: the cross-reference table was unusable; objects were recovered by scanning the file"
                    .to_string(),
            );
        } else if self.rescued {
            out.push(
                "pdf_repaired: some cross-reference offsets were wrong; objects were located by scanning the file"
                    .to_string(),
            );
        }
        // 文档的总页数：/Count 可能比子节点数多或少，按两者里大的算。content_missing、fonts_missing
        // 也用它作分母（以前用找到的页数，丢了页时分母会跟着变小）
        let total = self
            .declared_pages
            .unwrap_or(0)
            .max(found + self.missing_pages);
        if self.missing_pages > 0 {
            out.push(format!(
                "pages_missing: {} of {total} pages could not be recovered from the damaged file; {found} converted",
                total - found
            ));
        } else if self.page_count_unknown {
            out.push(format!(
                "pages_missing: the page tree was lost; {found} pages were recovered by scanning the file, but the total page count is unknown, so later pages may be missing"
            ));
        }
        if !self.incomplete_pages.is_empty() {
            let pages: Vec<String> = self
                .incomplete_pages
                .iter()
                .map(|l| {
                    if l.lost < l.streams {
                        format!("{} ({} of {} streams)", l.page, l.lost, l.streams)
                    } else {
                        l.page.to_string()
                    }
                })
                .collect();
            out.push(format!(
                "content_missing: {} of {total} pages lost content streams in the damaged file; their text is missing or incomplete (pages {})",
                self.incomplete_pages.len(),
                listed(&pages)
            ));
        }
        if !self.fontless_pages.is_empty() {
            let pages: Vec<String> = self.fontless_pages.iter().map(usize::to_string).collect();
            out.push(format!(
                "fonts_missing: {} of {total} pages lost font definitions in the damaged file; some characters may be decoded wrongly (pages {})",
                self.fontless_pages.len(),
                listed(&pages)
            ));
        }
        out
    }
}

/// 页码列表，最多 [`LISTED_PAGES`] 个，多了用省略号。
fn listed(pages: &[String]) -> String {
    let mut out: Vec<&str> = pages
        .iter()
        .take(LISTED_PAGES)
        .map(String::as_str)
        .collect();
    if pages.len() > LISTED_PAGES {
        out.push("…");
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intact_file_has_no_warnings() {
        assert!(Damage::default().warnings(5).is_empty());
    }

    #[test]
    fn repair_and_missing_pages_are_reported() {
        let d = Damage {
            repaired: true,
            rescued: true,
            declared_pages: Some(5),
            missing_pages: 2,
            ..Damage::default()
        };
        let w = d.warnings(3);
        assert_eq!(w.len(), 2);
        assert!(w[0].starts_with("pdf_repaired: the cross-reference table was unusable"));
        assert_eq!(
            w[1],
            "pages_missing: 2 of 5 pages could not be recovered from the damaged file; 3 converted"
        );
        // 偏移指错但 xref 还在：另一种措辞；/Count 撒谎时按子节点数算
        let d = Damage {
            repaired: false,
            rescued: true,
            declared_pages: Some(1),
            missing_pages: 1,
            ..Damage::default()
        };
        let w = d.warnings(3);
        assert!(w[0].contains("offsets were wrong"));
        assert!(w[1].starts_with("pages_missing: 1 of 4 pages"));
        // 只是 /Count 多报、没有丢对象：不警告
        let d = Damage {
            declared_pages: Some(9),
            ..Damage::default()
        };
        assert!(d.warnings(3).is_empty());
    }

    fn lost(page: usize, lost: usize, streams: usize) -> LostContent {
        LostContent {
            page,
            lost,
            streams,
        }
    }

    #[test]
    fn pages_with_lost_content_are_listed() {
        // 第 1 页两段内容流丢了一段（arXiv 加在文件末尾的侧边水印），第 4 页全丢
        let d = Damage {
            repaired: true,
            incomplete_pages: vec![lost(1, 1, 2), lost(4, 1, 1)],
            ..Damage::default()
        };
        let w = d.warnings(15);
        assert_eq!(
            w[1],
            "content_missing: 2 of 15 pages lost content streams in the damaged file; their text is missing or incomplete (pages 1 (1 of 2 streams), 4)"
        );
        let fonts = Damage {
            fontless_pages: vec![3],
            ..Damage::default()
        };
        assert!(fonts.warnings(15)[0].starts_with("fonts_missing: 1 of 15 pages"));
        let many = Damage {
            incomplete_pages: (1..=12).map(|p| lost(p, 1, 1)).collect(),
            ..Damage::default()
        };
        assert!(many.warnings(12)[0].ends_with("(pages 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, …)"));
    }

    #[test]
    fn the_denominator_is_the_document_not_what_was_recovered() {
        // 15 页的文件丢了 2 页：内容流丢失的分母仍是 15
        let d = Damage {
            declared_pages: Some(15),
            missing_pages: 2,
            incomplete_pages: vec![lost(1, 2, 2)],
            ..Damage::default()
        };
        let w = d.warnings(13);
        assert!(w[0].starts_with("pages_missing: 2 of 15 pages"), "{w:?}");
        assert!(w[1].starts_with("content_missing: 1 of 15 pages"), "{w:?}");
    }

    #[test]
    fn an_unknown_page_count_is_said_out_loud() {
        // 页树根丢了、哪里都没写总页数：找回 13 页，不能当作文件就是 13 页
        let d = Damage {
            repaired: true,
            page_count_unknown: true,
            ..Damage::default()
        };
        let w = d.warnings(13);
        assert!(
            w[1].starts_with("pages_missing: the page tree was lost; 13 pages were recovered"),
            "{w:?}"
        );
    }
}
