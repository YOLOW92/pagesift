//! 推断的文档标题：第一页排出的第一个一级标题。
//!
//! 很多 PDF（arXiv 的论文、扫描后转换的报告）/Info 里没有 /Title，也没有 XMP；正文里标题已经
//! 识别成了 `# ...`，调用方却拿不到 `metadata.title`。这里把它单独交出去，由调用方决定回填。
//!
//! 写入方给了章节标题（书签、结构树）却没给文档题名时，题名在按提示排版的模式下不是标题行：
//! [`candidate`] 按版式（第一页字号最大的那一短段）把它找出来，渲染时提成一级标题。

use std::cmp::Ordering;

use crate::layout::Para;

/// 第一页上版式最像文档题名的段落的下标：本页字号最大、比正文明显大、比写入方标了层级的标题都大、
/// 短（不超过 3 行、200 字）、写入方没给标题层级的那一段。`body` 是全文正文字号。
pub(crate) fn candidate(first_page: &[Para], body: f64) -> Option<usize> {
    let hinted = first_page
        .iter()
        .filter(|p| p.hint.is_some())
        .map(|p| p.size)
        .fold(0.0, f64::max);
    let (i, p) = first_page
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.raw && !p.turned && !p.bullet)
        .max_by(|a, b| {
            a.1.size
                .partial_cmp(&b.1.size)
                .unwrap_or(Ordering::Equal)
                .then(b.0.cmp(&a.0))
        })?;
    let n = p.text.chars().count();
    (p.hint.is_none()
        && p.size >= body * 1.25
        && p.size - body >= 2.0
        && p.size >= hinted + 1.0
        && p.lines <= 3
        && (3..=200).contains(&n)
        && p.text.chars().any(char::is_alphabetic))
    .then_some(i)
}

/// 第一页 Markdown 里的第一个一级标题；没有就 None。
pub(crate) fn inferred(first_page_markdown: &str) -> Option<String> {
    first_page_markdown
        .lines()
        .map(str::trim_end)
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(MAX_TITLE_CHARS).collect())
}

/// 标题行本来就不超过 200 个字符（见 `layout::render::is_heading_size`），再留点余量。
const MAX_TITLE_CHARS: usize = 300;

#[cfg(test)]
mod tests {
    use super::{candidate, inferred};
    use crate::layout::Para;

    fn para(text: &str, size: f64, lines: usize, hint: Option<u8>) -> Para {
        Para {
            text: text.into(),
            size,
            lines,
            hint,
            ..Para::default()
        }
    }

    #[test]
    fn largest_short_paragraph_on_the_first_page_is_the_title() {
        let page = vec![
            para("Provided proper attribution is given.", 9.0, 1, None),
            para("Attention Is All You Need", 17.0, 1, None),
            para("Ashish Vaswani", 10.0, 1, None),
            para("Abstract", 12.0, 1, Some(2)),
            para("The dominant sequence transduction models.", 10.0, 3, None),
        ];
        assert_eq!(candidate(&page, 10.0), Some(1));
        // 最大的那段太长（一整段大字的摘要）：不是题名
        let mut long = page.clone();
        long[1].lines = 6;
        assert_eq!(candidate(&long, 10.0), None);
        // 最大字号只比正文大一点：不猜
        let mut flat = page.clone();
        flat[1].size = 11.0;
        assert_eq!(candidate(&flat, 10.0), None);
        // 写入方标了层级的标题和它一样大（或更大）：照写入方的来
        let mut hinted = page.clone();
        hinted[3].size = 17.0;
        assert_eq!(candidate(&hinted, 10.0), None);
        // 书签标的章节标题比它小：它就是题名（书签常常只到章节）
        let mut sections = page.clone();
        sections[3].hint = Some(1);
        assert_eq!(candidate(&sections, 10.0), Some(1));
        // 最大的那段本身带提示（结构树标的二级标题）：照提示来
        let mut tagged = page;
        tagged[1].hint = Some(2);
        assert_eq!(candidate(&tagged, 10.0), None);
    }

    #[test]
    fn first_h1_of_the_first_page() {
        let md = "Provided proper attribution is given.\n\n# Attention Is All You Need\n\nAshish Vaswani*\n\n# 1 Introduction\n";
        assert_eq!(inferred(md).as_deref(), Some("Attention Is All You Need"));
        assert_eq!(inferred("## Only a subheading\n\nBody.\n"), None);
        assert_eq!(inferred("#\n"), None);
        assert_eq!(inferred(""), None);
    }
}
