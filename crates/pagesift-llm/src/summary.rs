use crate::client::{ChatClient, ChatMessage, ChatRequest, Role};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct SummaryOptions {
    /// 单次请求允许放进去的输入 token 估算值（上下文窗口减去输出余量）。
    pub max_input_tokens: usize,
    /// 摘要使用的语言，如 "中文"、"English"；缺省与原文一致。
    pub language: Option<String>,
    pub prompt: Option<String>,
}

impl Default for SummaryOptions {
    fn default() -> Self {
        Self {
            max_input_tokens: 6000,
            language: None,
            prompt: None,
        }
    }
}

/// 粗略估算 token 数：CJK 字符按 1 个，其余按 4 字节 1 个，偏保守。
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other_bytes = 0usize;
    for c in text.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other_bytes += c.len_utf8();
        }
    }
    cjk + other_bytes.div_ceil(4)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

/// 生成摘要。输入超过预算时先分段各自摘要，再合并摘要；
/// 合并后仍超长就继续归并，最多 4 轮，之后按预算截断。
pub async fn summarize(
    client: &ChatClient,
    markdown: &str,
    opts: &SummaryOptions,
) -> Result<String> {
    let budget = opts.max_input_tokens.max(1);
    let mut text = markdown.to_owned();
    for _ in 0..4 {
        if estimate_tokens(&text) <= budget {
            break;
        }
        let chunks = split_by_tokens(&text, budget);
        let mut parts = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            parts.push(summarize_once(client, chunk, opts).await?);
        }
        let merged = parts.join("\n\n");
        if estimate_tokens(&merged) >= estimate_tokens(&text) {
            text = merged;
            break;
        }
        text = merged;
    }
    if estimate_tokens(&text) > budget {
        text = truncate_tokens(&text, budget);
    }
    summarize_once(client, &text, opts).await
}

async fn summarize_once(client: &ChatClient, text: &str, opts: &SummaryOptions) -> Result<String> {
    let mut sys = String::from(
        "You summarize web page content given as Markdown. Write a concise, faithful summary \
without adding facts that are not in the text.",
    );
    if let Some(l) = &opts.language {
        sys.push_str(&format!(" Write the summary in {l}."));
    }
    let mut user = String::new();
    if let Some(p) = &opts.prompt {
        user.push_str(p);
        user.push_str("\n\n");
    }
    user.push_str(text);
    let req = ChatRequest {
        messages: vec![
            ChatMessage::new(Role::System, sys),
            ChatMessage::new(Role::User, user),
        ],
        temperature: Some(0.2),
        ..Default::default()
    };
    Ok(client.chat(&req).await?.content.trim().to_owned())
}

fn truncate_tokens(text: &str, budget: usize) -> String {
    let mut used = 0usize;
    let mut end = 0usize;
    for (i, c) in text.char_indices() {
        used += if is_cjk(c) { 4 } else { c.len_utf8() };
        if used > budget * 4 {
            break;
        }
        end = i + c.len_utf8();
    }
    text[..end].to_owned()
}

/// 按空行切段后贪心装箱；单段过长时按字符硬切。
fn split_by_tokens(text: &str, budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, chunks: &mut Vec<String>| {
        if !cur.trim().is_empty() {
            chunks.push(std::mem::take(cur));
        }
        cur.clear();
    };
    for para in text.split("\n\n") {
        let mut para = para.to_owned();
        while estimate_tokens(&para) > budget {
            let head = truncate_tokens(&para, budget);
            if head.is_empty() {
                break;
            }
            let rest = para[head.len()..].to_owned();
            flush(&mut cur, &mut chunks);
            chunks.push(head);
            para = rest;
        }
        if estimate_tokens(&cur) + estimate_tokens(&para) + 1 > budget {
            flush(&mut cur, &mut chunks);
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(&para);
    }
    flush(&mut cur, &mut chunks);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates() {
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        assert_eq!(estimate_tokens("你好世界"), 4);
    }

    #[test]
    fn splits_within_budget() {
        let text = (0..50)
            .map(|i| format!("paragraph {i} {}", "x".repeat(60)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = split_by_tokens(&text, 100);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| estimate_tokens(c) <= 100));
    }

    #[test]
    fn splits_giant_paragraph() {
        let chunks = split_by_tokens(&"a".repeat(2000), 100);
        assert!(chunks.len() >= 5);
        assert!(chunks.iter().all(|c| estimate_tokens(c) <= 100));
    }
}
