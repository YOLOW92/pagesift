//! 哪些 warning 要让调用方直接看到：结果可能不完整、不对，或者根本不是想要的页面。
//!
//! 所有 warning 都在 `metadata.pagesift.warnings` 里；这里挑出的几类另外汇成一句话写进 Firecrawl 的
//! `data.warning`（流水线按 Firecrawl 格式消费时只看这个字段），命令行也把它们打到 stderr。
//! 两处用同一份名单，不会一处有一处没有（0.80.3 的 `content_missing` 只出现在 stderr）。

use pagesift_api::types::Document;

/// 按前缀匹配的 warning 类型。
pub const CONTENT_RISK: &[&str] = &[
    // 正文空、薄、只抽到一部分
    "empty_content",
    "thin_content",
    "partial_content",
    "replies_omitted",
    // 文件损坏或没转完
    "pages_missing",
    "content_missing",
    "fonts_missing",
    "max_pages",
    "scanned PDF",
    "decode",
    "response body exceeded the size limit",
    // 拿到的不是正常的 200 页面
    "http_error_status",
    "http_redirect_status",
    "not_modified",
    // 需要浏览器渲染却没有渲染
    "render_unavailable",
    "render_skipped_",
];

/// 这条 warning 是不是要让调用方直接看到的那类。
pub fn is_content_risk(warning: &str) -> bool {
    CONTENT_RISK.iter().any(|kind| warning.starts_with(kind))
}

/// 把要让调用方看到的 warning 汇成 `data.warning`；已经有值时接在后面。
pub fn fill_warning(doc: &mut Document) {
    let Some(p) = doc.metadata.pagesift.as_ref() else {
        return;
    };
    let risks: Vec<&str> = p
        .warnings
        .iter()
        .map(String::as_str)
        .filter(|w| is_content_risk(w))
        .collect();
    if risks.is_empty() {
        return;
    }
    let joined = risks.join("; ");
    doc.warning = Some(match doc.warning.take() {
        Some(existing) if !existing.is_empty() => format!("{existing}; {joined}"),
        _ => joined,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_api::types::{Metadata, PagesiftMeta};

    fn doc(warnings: &[&str]) -> Document {
        Document {
            metadata: Metadata {
                pagesift: Some(PagesiftMeta {
                    warnings: warnings.iter().map(|w| w.to_string()).collect(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn data_loss_warnings_reach_the_firecrawl_field() {
        let mut d = doc(&[
            "pdf_repaired: the cross-reference table was unusable",
            "content_missing: 2 of 15 pages lost their content streams (pages 1, 4)",
            "fonts_missing: 1 of 15 pages lost font definitions (pages 2)",
        ]);
        fill_warning(&mut d);
        assert_eq!(
            d.warning.as_deref(),
            Some(
                "content_missing: 2 of 15 pages lost their content streams (pages 1, 4); \
                 fonts_missing: 1 of 15 pages lost font definitions (pages 2)"
            )
        );
    }

    #[test]
    fn informational_warnings_stay_in_metadata_only() {
        let mut d = doc(&[
            "pdf_repaired: recovered",
            "comments_omitted: 300 characters",
        ]);
        fill_warning(&mut d);
        assert_eq!(d.warning, None);
    }

    #[test]
    fn audit_snapshots_cover_only_content_risks() {
        for kind in crate::audit::SUSPICIOUS_WARNINGS {
            assert!(is_content_risk(kind), "{kind}");
        }
    }
}
