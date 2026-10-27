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
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Damage {
    /// xref 不可用，对象索引是扫描全文重建的。
    pub(crate) repaired: bool,
    /// xref 可用但有偏移指错的对象，靠扫描全文找到。
    pub(crate) rescued: bool,
    pub(crate) declared_pages: Option<usize>,
    pub(crate) missing_pages: usize,
}

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
        if self.missing_pages > 0 {
            // /Count 可能比子节点数多或少；按两者里大的算总数
            let total = self
                .declared_pages
                .unwrap_or(0)
                .max(found + self.missing_pages);
            out.push(format!(
                "pages_missing: {} of {total} pages could not be recovered from the damaged file; {found} converted",
                total - found
            ));
        }
        out
    }
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
}
