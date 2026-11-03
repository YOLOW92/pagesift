//! 按 Jina 的输出参数写响应正文：`x-respond-with` 的各种形状、图片/链接/媒体的保留方式、
//! 链接和图片汇总、`x-max-tokens` 截断（报 `output_truncated`）、`x-token-budget` 超限拒绝。
//! token 数和分块模块用同一个 `TokenIndex`。

use regex::Regex;
use std::sync::LazyLock;

use super::headers::{JinaParams, RespondWith, Retain};
use crate::error::{ApiError, ErrorCode};
use crate::types::Document;

static IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#).expect("regex"));
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[^!])\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#).expect("regex")
});

/// 视频、音频：文件扩展名，或常见视频站的观看/嵌入地址。
fn is_media(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    const EXT: &[&str] = &[
        ".mp4", ".webm", ".mov", ".m4v", ".ogv", ".mp3", ".m4a", ".ogg", ".oga", ".wav", ".flac",
        ".m3u8", ".mpd",
    ];
    const HOSTS: &[&str] = &[
        "youtube.com/watch",
        "youtube.com/embed/",
        "youtu.be/",
        "vimeo.com/",
        "player.vimeo.com/",
        "bilibili.com/video/",
        "player.bilibili.com/",
    ];
    EXT.iter().any(|e| path.ends_with(e)) || HOSTS.iter().any(|h| path.contains(h))
}

/// 汇总里的一项：文字和地址。
type Ref = (String, String);

/// 应用保留方式，并收集汇总用的链接和图片（去掉之前收集）。
pub fn reshape(md: &str, p: &JinaParams) -> (String, Vec<Ref>, Vec<Ref>) {
    let images: Vec<Ref> = IMAGE
        .captures_iter(md)
        .filter(|c| !is_media(&c[2]))
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    let links: Vec<Ref> = LINK
        .captures_iter(md)
        .filter(|c| !is_media(&c[3]))
        .map(|c| (c[2].to_string(), c[3].to_string()))
        .collect();
    let out = IMAGE.replace_all(md, |c: &regex::Captures<'_>| {
        let (alt, url) = (&c[1], &c[2]);
        let retain = if is_media(url) {
            p.retain_media
        } else {
            p.retain_images
        };
        match retain {
            Retain::All | Retain::Image => c[0].to_string(),
            Retain::None => String::new(),
            Retain::Text => alt.to_string(),
        }
    });
    let out = LINK.replace_all(&out, |c: &regex::Captures<'_>| {
        let (lead, text, url) = (&c[1], &c[2], &c[3]);
        if is_media(url) {
            return match p.retain_media {
                Retain::All => c[0].to_string(),
                Retain::None => lead.to_string(),
                Retain::Text => format!("{lead}{text}"),
                Retain::Image => format!("{lead}![{text}]({url})"),
            };
        }
        match p.retain_links {
            Retain::All | Retain::Image => c[0].to_string(),
            Retain::None => lead.to_string(),
            Retain::Text => format!("{lead}{text}"),
        }
    });
    (out.into_owned(), links, images)
}

fn summary(title: &str, refs: &[Ref], all: bool, image: bool) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut s = format!("\n\n{title}:\n");
    for (text, url) in refs {
        if !all && !seen.insert(url.clone()) {
            continue;
        }
        if image {
            s.push_str(&format!("- ![{text}]({url})\n"));
        } else {
            s.push_str(&format!("- [{text}]({url})\n"));
        }
    }
    s
}

/// 粗略的 Markdown → 纯文本：去掉标题井号、强调符号、链接和图片的语法（保留文字）。
pub fn markdown_text(md: &str) -> String {
    let no_img = IMAGE.replace_all(md, "$1");
    let no_link = LINK.replace_all(&no_img, "$1$2");
    no_link
        .lines()
        .map(|l| {
            l.trim_start_matches('#')
                .trim_start()
                .replace("**", "")
                .replace("__", "")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn token_count(s: &str) -> usize {
    pagesift_extract::TokenIndex::new(s).len()
}

/// 截到 `max` 个 token（在 token 边界）；返回截断后的文本和是否截断了。
pub fn truncate_tokens(s: &str, max: usize) -> (String, bool) {
    let idx = pagesift_extract::TokenIndex::new(s);
    match idx.starts.get(max) {
        Some(&cut) => (s[..cut].trim_end().to_string(), true),
        None => (s.to_string(), false),
    }
}

fn yaml_str(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into())
}

/// 响应：内容类型、正文、警告。
#[derive(Debug)]
pub struct Rendered {
    pub content_type: &'static str,
    pub body: String,
    /// 纯文本响应：不分块时同 `body`；分块时是 front matter、用分隔线连起来的各块、汇总。
    pub text: String,
    pub warnings: Vec<String>,
    /// 分块时的各块（`Accept: application/json` 时返回数组）。
    pub chunks: Option<Vec<String>>,
}

pub fn render(doc: &Document, url: &str, p: &JinaParams) -> Result<Rendered, ApiError> {
    if let Some(want) = p.assert_status
        && doc.metadata.status_code != want
    {
        return Err(ApiError::new(
            ErrorCode::AssertionFailed,
            format!(
                "the page answered {}, not the asserted {want}",
                doc.metadata.status_code
            ),
        ));
    }
    let title = doc.metadata.title.as_deref().unwrap_or_default();
    let mut warnings = p.notes.clone();
    // 分块时纯文本响应的开头和结尾（front matter、汇总）：块替换的只是正文
    let (mut head, mut tail) = (String::new(), String::new());
    let (content_type, body) = match p.respond_with {
        RespondWith::Html => (
            "text/html; charset=utf-8",
            doc.raw_html.clone().unwrap_or_default(),
        ),
        RespondWith::Screenshot { .. } => {
            let shot = doc
                .actions
                .as_ref()
                .and_then(|a| a["screenshots"].get(0))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ApiError::unsupported(
                        "screenshot needs browser rendering, which is not available here",
                    )
                })?;
            ("text/plain; charset=utf-8", shot.to_string())
        }
        _ => {
            let md = doc.markdown.clone().unwrap_or_default();
            let (mut md, links, images) = reshape(&md, p);
            if let Some(all) = p.images_summary {
                tail.push_str(&summary("Images", &images, all, true));
            }
            if let Some(all) = p.links_summary {
                tail.push_str(&summary("Links/Buttons", &links, all, false));
            }
            md.push_str(&tail);
            match p.respond_with {
                RespondWith::Text => ("text/plain; charset=utf-8", markdown_text(&md)),
                RespondWith::Frontmatter | RespondWith::MarkdownFrontmatter => {
                    let desc = doc.metadata.description.as_deref().unwrap_or_default();
                    head = format!(
                        "---\ntitle: {}\ndescription: {}\nurl: {}\n---\n\n",
                        yaml_str(title),
                        yaml_str(desc),
                        yaml_str(url)
                    );
                    ("text/markdown; charset=utf-8", format!("{head}{md}"))
                }
                _ => ("text/markdown; charset=utf-8", md),
            }
        }
    };
    let mut body = body;
    if let Some(budget) = p.token_budget {
        let n = token_count(&body);
        if n > budget {
            return Err(ApiError::new(
                ErrorCode::TooLarge,
                format!("the content has {n} tokens, over x-token-budget {budget}"),
            )
            .with_details(serde_json::json!({ "tokens": n, "budget": budget })));
        }
    }
    // 分块来自原 Markdown：每块同样应用保留方式；纯文本时块里也要是纯文本
    let mut chunks = p.chunking.as_ref().and(doc.chunks.as_ref()).map(|c| {
        c.iter()
            .map(|ch| {
                let md = reshape(&ch.text, p).0;
                match p.respond_with {
                    RespondWith::Text => markdown_text(&md),
                    _ => md,
                }
            })
            .collect::<Vec<_>>()
    });
    if let Some(max) = p.max_tokens {
        let (cut, truncated) = truncate_tokens(&body, max);
        if truncated {
            warnings.push(format!(
                "output_truncated: {} of {} tokens",
                max,
                token_count(&body)
            ));
            body = cut;
        }
        // 块和正文共用同一个上限：超出的块丢掉，跨线的那块截断
        if let Some(c) = &mut chunks {
            truncate_chunks(c, max);
        }
    }
    let text = match &chunks {
        Some(c) => {
            let (joined, _) = truncate_tokens(
                &format!("{head}{}{tail}", c.join(CHUNK_SEPARATOR)),
                p.max_tokens.unwrap_or(usize::MAX),
            );
            joined
        }
        None => body.clone(),
    };
    Ok(Rendered {
        content_type,
        body,
        text,
        warnings,
        chunks,
    })
}

/// 纯文本分块响应里块与块之间的分隔：一个空行加 `---` 再一个空行（Markdown 的分隔线，不和正文混淆）。
const CHUNK_SEPARATOR: &str = "\n\n---\n\n";

/// 让所有块加起来不超过 `max` 个 token。
fn truncate_chunks(chunks: &mut Vec<String>, max: usize) {
    let mut left = max;
    let mut keep = 0;
    for c in chunks.iter_mut() {
        if left == 0 {
            break;
        }
        let n = token_count(c);
        if n > left {
            *c = truncate_tokens(c, left).0;
            keep += 1;
            break;
        }
        left -= n;
        keep += 1;
    }
    chunks.truncate(keep);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jina::headers::parse;
    use axum::http::HeaderMap;

    fn params(pairs: &[(&str, &str)]) -> JinaParams {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        parse(&h).unwrap().1
    }

    const MD: &str = "# T\n\nSee [docs](https://a.test/docs) and [docs again](https://a.test/docs).\n\n![logo](https://a.test/l.png) [clip](https://youtu.be/x1)";

    #[test]
    fn retain_modes_and_summaries() {
        let (out, ..) = reshape(
            MD,
            &params(&[
                ("x-retain-links", "text"),
                ("x-retain-images", "none"),
                ("x-retain-media", "none"),
            ]),
        );
        assert_eq!(out, "# T\n\nSee docs and docs again.\n\n ");
        let p = params(&[("x-retain-images", "alt"), ("x-with-links-summary", "true")]);
        let doc = Document {
            markdown: Some(MD.into()),
            ..Default::default()
        };
        let r = render(&doc, "https://a.test/", &p).unwrap();
        assert!(
            r.body.contains("\n\nlogo [clip](https://youtu.be/x1)"),
            "{}",
            r.body
        );
        assert_eq!(
            r.body.matches("- [docs").count(),
            1,
            "deduplicated: {}",
            r.body
        );
    }

    fn chunk(text: &str) -> crate::types::DocChunk {
        crate::types::DocChunk {
            text: text.into(),
            heading_path: Vec::new(),
            start: 0,
            end: 0,
            tokens: 0,
        }
    }

    fn chunked_doc() -> Document {
        Document {
            markdown: Some(
                "## A\n\nalpha words\n\n## B\n\nbeta words [l](https://a.test/x)".into(),
            ),
            chunks: Some(vec![
                chunk("## A\n\nalpha words"),
                chunk("## B\n\nbeta words [l](https://a.test/x)"),
            ]),
            ..Default::default()
        }
    }

    #[test]
    fn chunks_keep_frontmatter_summary_and_limit() {
        let p = params(&[
            ("x-respond-with", "frontmatter"),
            ("x-markdown-chunking", "h2"),
            ("x-with-links-summary", "true"),
        ]);
        let r = render(&chunked_doc(), "https://a.test/", &p).unwrap();
        assert!(r.text.starts_with("---\ntitle:"), "{}", r.text);
        assert!(r.text.contains("alpha words\n\n---\n\n## B"), "{}", r.text);
        assert!(r.text.contains("Links/Buttons"), "{}", r.text);
        assert_eq!(r.chunks.as_ref().unwrap().len(), 2);
        let p = params(&[("x-markdown-chunking", "h2"), ("x-max-tokens", "4")]);
        let r = render(&chunked_doc(), "u", &p).unwrap();
        assert!(
            r.warnings[0].starts_with("output_truncated"),
            "{:?}",
            r.warnings
        );
        let c = r.chunks.unwrap();
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(token_count(&r.text) <= 4, "{}", r.text);
    }

    #[test]
    fn asserted_status_and_preset_notes() {
        let mut doc = chunked_doc();
        doc.metadata.status_code = 404;
        let e = render(&doc, "u", &params(&[("x-assert-status-code", "200")])).unwrap_err();
        assert_eq!(e.code, ErrorCode::AssertionFailed);
        assert!(render(&doc, "u", &params(&[("x-assert-status-code", "404")])).is_ok());
        let r = render(&doc, "u", &params(&[("x-preset", "reader")])).unwrap();
        assert!(
            r.warnings[0].starts_with("preset_partial"),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn max_tokens_truncates_and_budget_refuses() {
        let doc = Document {
            markdown: Some("one two three four five six".into()),
            ..Default::default()
        };
        let r = render(&doc, "u", &params(&[("x-max-tokens", "3")])).unwrap();
        assert_eq!(r.body, "one two three");
        assert!(r.warnings[0].starts_with("output_truncated"));
        let e = render(&doc, "u", &params(&[("x-token-budget", "5")])).unwrap_err();
        assert_eq!(e.code, ErrorCode::TooLarge);
        let ok = render(&doc, "u", &params(&[("x-respond-with", "frontmatter")])).unwrap();
        assert!(ok.body.starts_with("---\ntitle: \"\"\n"));
    }
}
