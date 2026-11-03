//! 块：Markdown 按空行切成的最小合并单位。
//!
//! 和 `pagesift_diff::split_blocks` 不同，这里一块保留原文的完整排版（整张表、整个列表、整段代码），
//! 合并后按原文拼回去，Markdown 结构不变。指纹只依赖规范化文本（不变量 4），与执行器无关。

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Heading,
    Paragraph,
    List,
    Table,
    Code,
    Quote,
    Image,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: Kind,
    /// 原文（不含前后空行）。
    pub text: String,
    /// 规范化文本的 BLAKE3 指纹的前 8 字节。
    pub fingerprint: u64,
    /// 族内模板概率（由模板模型给出）；没有时为 `None`。
    pub template_prob: Option<f64>,
}

impl Block {
    pub fn new(kind: Kind, text: String) -> Self {
        let norm = pagesift_diff::normalize(&text, kind == Kind::Code);
        let hash = blake3::hash(norm.as_bytes());
        // [协议] 指纹取 BLAKE3 摘要的前 8 字节（一个 u64）
        let (first, _) = hash
            .as_bytes()
            .split_first_chunk::<8>()
            .expect("BLAKE3 digest is 32 bytes");
        Self {
            kind,
            text,
            fingerprint: u64::from_le_bytes(*first),
            template_prob: None,
        }
    }

    /// 词集合（小写 `\w+`），算相似度用。
    pub fn words(&self) -> HashSet<String> {
        self.text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    }
}

fn kind_of(first_line: &str) -> Kind {
    let t = first_line.trim_start();
    if t.starts_with('#') {
        Kind::Heading
    } else if t.starts_with("```") || t.starts_with("~~~") {
        Kind::Code
    } else if t.starts_with('|') {
        Kind::Table
    } else if t.starts_with('>') {
        Kind::Quote
    } else if t.starts_with("![") {
        Kind::Image
    } else if t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
        || t.split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        Kind::List
    } else {
        Kind::Paragraph
    }
}

/// 按空行切块；围栏代码块里的空行不切；标题单独成块。
pub fn split(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;
    let flush = |cur: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !cur.is_empty() {
            out.push(Block::new(kind_of(cur[0]), cur.join("\n")));
            cur.clear();
        }
    };
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(f) = fence {
            cur.push(line);
            if t.starts_with(f) {
                fence = None;
                flush(&mut cur, &mut out);
            }
            continue;
        }
        if t.is_empty() {
            flush(&mut cur, &mut out);
        } else if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut cur, &mut out);
            fence = Some(&t[..3]); // [协议] CommonMark 围栏至少 3 个字符
            cur.push(line);
        } else if t.starts_with('#') {
            flush(&mut cur, &mut out);
            cur.push(line);
            flush(&mut cur, &mut out);
        } else {
            cur.push(line);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// 块拼回 Markdown（块间一个空行）。
pub fn join<'a>(blocks: impl IntoIterator<Item = &'a Block>) -> String {
    let mut out = String::new();
    for b in blocks {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&b.text);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// 两块的 Jaccard 相似度（词集合）。块数每页几十到几百，精确计算比 MinHash 估计便宜也更准。
pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count();
    inter as f64 / (a.len() + b.len() - inter) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_tables_lists_and_code_whole_and_round_trips() {
        let md = "# T\n\nIntro text.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- x\n- y\n\n```\ncode\n\nmore\n```\n\nEnd.\n";
        let b = split(md);
        let kinds: Vec<Kind> = b.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Heading,
                Kind::Paragraph,
                Kind::Table,
                Kind::List,
                Kind::Code,
                Kind::Paragraph
            ]
        );
        assert_eq!(join(&b), md);
        // 指纹只看规范化文本
        assert_eq!(
            Block::new(Kind::Paragraph, "a  b".into()).fingerprint,
            Block::new(Kind::Paragraph, "a b".into()).fingerprint
        );
    }

    #[test]
    fn jaccard_is_intersection_over_union() {
        let a = Block::new(Kind::Paragraph, "one two three".into()).words();
        let b = Block::new(Kind::Paragraph, "two three four".into()).words();
        assert!((jaccard(&a, &b) - 0.5).abs() < 1e-12);
        assert_eq!(jaccard(&a, &a), 1.0);
    }
}
