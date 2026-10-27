//! 推断的文档标题：第一页排出的第一个一级标题。
//!
//! 很多 PDF（arXiv 的论文、扫描后转换的报告）/Info 里没有 /Title，也没有 XMP；正文里标题已经
//! 识别成了 `# ...`，调用方却拿不到 `metadata.title`。这里把它单独交出去，由调用方决定回填。

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
    use super::inferred;

    #[test]
    fn first_h1_of_the_first_page() {
        let md = "Provided proper attribution is given.\n\n# Attention Is All You Need\n\nAshish Vaswani*\n\n# 1 Introduction\n";
        assert_eq!(inferred(md).as_deref(), Some("Attention Is All You Need"));
        assert_eq!(inferred("## Only a subheading\n\nBody.\n"), None);
        assert_eq!(inferred("#\n"), None);
        assert_eq!(inferred(""), None);
    }
}
