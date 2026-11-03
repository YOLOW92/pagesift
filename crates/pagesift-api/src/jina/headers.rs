//! Jina Reader 的请求头 → pagesift 的抓取选项和输出参数（照 jina-ai/reader README 的“Using request
//! headers”逐条映射）。认识但做不到的报 `UNSUPPORTED_OPTION`，不悄悄忽略；不认识的 `x-` 头不报错
//! （中间层常加自己的头）。

use axum::http::HeaderMap;
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::types::{Format, ScrapeOptions};

/// `x-respond-with`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespondWith {
    /// 没给：pagesift 一直以来的输出（纯 Markdown）。
    Default,
    Markdown,
    Html,
    Text,
    /// YAML front matter（title、description、url）+ 正文 Markdown。
    Frontmatter,
    /// 同上，但不做正文识别（整页）。
    MarkdownFrontmatter,
    /// 截图（视口 / 整页），返回 `data:` URL。
    Screenshot {
        full_page: bool,
    },
}

/// `x-retain-images`、`x-retain-links`、`x-retain-media` 的取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retain {
    All,
    None,
    /// 图片只留 alt、链接只留锚文字、媒体只留标签。
    Text,
    /// 媒体改成图片语法。
    Image,
}

/// `x-markdown-chunking`。
#[derive(Debug, Clone, PartialEq)]
pub enum Chunking {
    Headings(u8),
    /// `s1`…`s5`：块级结构切分，`s1` 最粗。
    Structured(u8),
}

/// 除抓取选项之外，决定怎样写响应的参数。
#[derive(Debug, Clone)]
pub struct JinaParams {
    pub respond_with: RespondWith,
    pub max_tokens: Option<usize>,
    pub token_budget: Option<usize>,
    pub retain_images: Retain,
    pub retain_links: Retain,
    pub retain_media: Retain,
    /// `Some(false)` 去重，`Some(true)` 全部（`x-with-links-summary: all`）。
    pub links_summary: Option<bool>,
    pub images_summary: Option<bool>,
    pub chunking: Option<Chunking>,
}

fn get<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn bad(header: &str, value: &str, allowed: &str) -> ApiError {
    ApiError::invalid(format!(
        "{header}: unsupported value {value:?} (allowed: {allowed})"
    ))
}

fn truthy(v: Option<&str>) -> bool {
    v.is_some_and(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no"))
}

fn uint(h: &HeaderMap, name: &str) -> Result<Option<u64>, ApiError> {
    get(h, name)
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| ApiError::invalid(format!("{name} must be a non-negative integer")))
        })
        .transpose()
}

/// `x-preset` 的选项包：只填调用方没有显式给的项（照 Jina cookbooks.md 的表）。
fn preset(name: &str) -> Result<&'static [(&'static str, &'static str)], ApiError> {
    Ok(match name {
        "reader" => &[
            ("x-respond-with", "frontmatter"),
            ("x-retain-media", "html"),
        ],
        "index" => &[
            ("x-retain-links", "text"),
            ("x-retain-images", "alt"),
            ("x-retain-media", "none"),
            ("x-markdown-chunking", "s3"),
        ],
        "research" => &[
            ("x-respond-with", "markdown+frontmatter"),
            ("x-markdown-chunking", "h3"),
        ],
        "agent" => &[
            ("x-respond-with", "frontmatter"),
            ("x-markdown-chunking", "h3"),
            ("x-retain-images", "alt"),
        ],
        "spider" => &[
            ("x-respond-with", "markdown+frontmatter"),
            ("x-markdown-chunking", "h3"),
            ("x-with-links-summary", "all"),
        ],
        other => {
            return Err(bad(
                "x-preset",
                other,
                "reader, index, research, agent, spider",
            ));
        }
    })
}

/// 解析请求头。`x-proxy-url` 只接受配置里的出口名（交给引擎按名字查，查不到就拒绝），
/// 任意代理地址一律拒绝，防止借道打内网。
pub fn parse(headers: &HeaderMap) -> Result<(ScrapeOptions, JinaParams), ApiError> {
    // 预设只补没给的头：先把预设展开成一份头表，调用方的头覆盖它
    let mut h = HeaderMap::new();
    if let Some(p) = get(headers, "x-preset") {
        for (k, v) in preset(&p.to_ascii_lowercase())? {
            h.insert(*k, v.parse().expect("static header value"));
        }
    }
    for (k, v) in headers {
        h.insert(k.clone(), v.clone());
    }
    let h = &h;
    let mut opts = ScrapeOptions::default();

    let respond_with = match get(h, "x-respond-with")
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None => RespondWith::Default,
        Some("markdown") => RespondWith::Markdown,
        Some("html") => RespondWith::Html,
        Some("text") => RespondWith::Text,
        Some("frontmatter") => RespondWith::Frontmatter,
        Some("markdown+frontmatter") => RespondWith::MarkdownFrontmatter,
        Some("screenshot") => RespondWith::Screenshot { full_page: false },
        Some("pageshot") => RespondWith::Screenshot { full_page: true },
        Some(v @ ("vlm" | "readerlm-v2" | "content")) => {
            return Err(ApiError::unsupported(format!(
                "x-respond-with: {v} is not supported (it needs a hosted model); use markdown, html, text, frontmatter, markdown+frontmatter, screenshot or pageshot"
            )));
        }
        Some(v) => {
            return Err(bad(
                "x-respond-with",
                v,
                "markdown, html, text, frontmatter, markdown+frontmatter, screenshot, pageshot",
            ));
        }
    };
    match respond_with {
        // Jina 的 markdown、html、text 都是整页（不经 readability）；frontmatter 是正文
        RespondWith::Markdown
        | RespondWith::MarkdownFrontmatter
        | RespondWith::Html
        | RespondWith::Text => {
            opts.only_main_content = false;
        }
        _ => {}
    }
    if respond_with == RespondWith::Html {
        opts.formats = vec![Format::new("rawHtml")];
    }
    if let RespondWith::Screenshot { full_page } = respond_with {
        opts.engine = Some("browser".into());
        opts.actions = vec![json!({ "type": "screenshot", "fullPage": full_page })];
    }

    if let Some(e) = get(h, "x-engine") {
        opts.engine = Some(
            match e.to_ascii_lowercase().as_str() {
                "browser" => "browser",
                "curl" | "direct" => "http",
                "auto" => "auto",
                other => return Err(bad("x-engine", other, "browser, curl, direct, auto")),
            }
            .into(),
        );
    }
    if let Some(sel) = get(h, "x-target-selector") {
        opts.include_tags = vec![sel.to_string()];
        opts.only_main_content = false;
    }
    if let Some(sel) = get(h, "x-remove-selector") {
        opts.exclude_tags = vec![sel.to_string()];
    }
    if let Some(sel) = get(h, "x-wait-for-selector") {
        opts.actions
            .insert(0, json!({ "type": "wait", "selector": sel }));
        opts.engine.get_or_insert_with(|| "browser".into());
    }
    let timeout = uint(h, "x-timeout")?;
    if let Some(t) = timeout {
        opts.timeout = Some(t.saturating_mul(1000));
    }
    if let Some(t) = get(h, "x-respond-timing") {
        let t = t.to_ascii_lowercase();
        if !crate::types::RESPOND_TIMINGS.contains(&t.as_str()) {
            return Err(bad(
                "x-respond-timing",
                &t,
                &crate::types::RESPOND_TIMINGS.join(", "),
            ));
        }
        // html 不执行脚本：直接 HTTP；其余档位要等页面上的变化，只有浏览器做得到
        if t == "html" {
            opts.engine.get_or_insert_with(|| "http".into());
        } else {
            opts.engine.get_or_insert_with(|| "browser".into());
        }
        opts.respond_timing = Some(t);
    } else if timeout.is_some_and(|t| t >= JINA_NETWORK_IDLE_TIMEOUT_SECS) {
        // Jina：x-timeout ≥ 20 时隐含 network-idle
        opts.respond_timing = Some("network-idle".into());
    }
    if truthy(get(h, "x-no-cache")) {
        opts.max_age = Some(0);
    }
    if let Some(s) = uint(h, "x-cache-tolerance")? {
        opts.max_age = Some(s.saturating_mul(1000));
    }
    if let Some(c) = get(h, "x-set-cookie") {
        // Jina 把 Set-Cookie 形式的值转成请求 Cookie；带 Cookie 的请求不读写缓存
        let pairs: Vec<&str> = c
            .split(',')
            .filter_map(|one| one.split(';').next())
            .map(str::trim)
            .filter(|kv| kv.contains('='))
            .collect();
        opts.headers.insert("Cookie".into(), pairs.join("; "));
        opts.max_age = Some(0);
        opts.store_in_cache = false;
    }
    if let Some(p) = get(h, "x-proxy-url").or_else(|| get(h, "x-proxy")) {
        if p.contains("://") || p.contains('@') || p.contains(':') {
            return Err(ApiError::invalid(format!(
                "x-proxy-url: only egress names configured in pagesift.toml are accepted (got {p:?}); arbitrary proxy URLs are refused so requests cannot be routed into internal networks"
            )));
        }
        opts.egress = Some(p.to_string());
    }
    if truthy(get(h, "x-with-generated-alt")) {
        return Err(ApiError::unsupported(
            "x-with-generated-alt is not supported: pagesift does not caption images with a vision model",
        ));
    }
    if truthy(get(h, "x-detach-invisibles")) {
        return Err(ApiError::unsupported(
            "x-detach-invisibles is not supported",
        ));
    }
    if get(h, "x-with-iframe").is_some() || get(h, "x-with-shadow-dom").is_some() {
        // 浏览器渲染本来就展开 iframe 和 shadow DOM；HTTP 抓取做不到
        opts.engine.get_or_insert_with(|| "browser".into());
    }

    let retain = |name: &str, allowed: &[(&str, Retain)]| -> Result<Retain, ApiError> {
        match get(h, name).map(str::to_ascii_lowercase) {
            None => Ok(Retain::All),
            Some(v) => allowed
                .iter()
                .find(|(k, _)| *k == v)
                .map(|(_, r)| *r)
                .ok_or_else(|| {
                    let names: Vec<&str> = allowed.iter().map(|(k, _)| *k).collect();
                    bad(name, &v, &names.join(", "))
                }),
        }
    };
    let retain_images = retain(
        "x-retain-images",
        &[
            ("all", Retain::All),
            ("none", Retain::None),
            ("alt", Retain::Text),
        ],
    )?;
    if get(h, "x-retain-links").is_some_and(|v| v.eq_ignore_ascii_case("gpt-oss")) {
        return Err(ApiError::unsupported(
            "x-retain-links: gpt-oss citations are not supported; use all, none or text",
        ));
    }
    let retain_links = retain(
        "x-retain-links",
        &[
            ("all", Retain::All),
            ("none", Retain::None),
            ("text", Retain::Text),
        ],
    )?;
    if get(h, "x-retain-media").is_some_and(|v| v.eq_ignore_ascii_case("html")) {
        return Err(ApiError::unsupported(
            "x-retain-media: html is not supported (the original media element is not kept); use link, none, text or image",
        ));
    }
    let retain_media = retain(
        "x-retain-media",
        &[
            ("link", Retain::All),
            ("none", Retain::None),
            ("text", Retain::Text),
            ("image", Retain::Image),
        ],
    )?;
    let summary = |name: &str| match get(h, name).map(str::to_ascii_lowercase).as_deref() {
        None | Some("false" | "0") => None,
        Some("all") => Some(true),
        Some(_) => Some(false),
    };
    let chunking = match get(h, "x-markdown-chunking")
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("false") => None,
        Some("true") => Some(Chunking::Headings(JINA_DEFAULT_HEADING_LEVEL)),
        Some("structured") => Some(Chunking::Structured(JINA_DEFAULT_STRUCTURED_LEVEL)),
        Some(v) => {
            let level = |p: char| {
                v.strip_prefix(p)
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (1..=5).contains(n)) // [协议] Jina 的 h1…h5、s1…s5
            };
            match (level('h'), level('s')) {
                (Some(n), _) => Some(Chunking::Headings(n)),
                (_, Some(n)) => Some(Chunking::Structured(n)),
                _ => {
                    return Err(bad(
                        "x-markdown-chunking",
                        v,
                        "true, h1…h5, structured, s1…s5",
                    ));
                }
            }
        }
    };
    if let Some(c) = &chunking {
        opts.chunking = Some(chunk_spec(c));
    }
    Ok((
        opts,
        JinaParams {
            respond_with,
            max_tokens: uint(h, "x-max-tokens")?.map(|n| n as usize),
            token_budget: uint(h, "x-token-budget")?.map(|n| n as usize),
            retain_images,
            retain_links,
            retain_media,
            links_summary: summary("x-with-links-summary"),
            images_summary: summary("x-with-images-summary"),
            chunking,
        },
    ))
}

/// [协议] Jina README：`x-timeout` ≥ 20 秒隐含 `network-idle`。
const JINA_NETWORK_IDLE_TIMEOUT_SECS: u64 = 20;
/// [协议] Jina README：`x-markdown-chunking: true` 等同 `h3`…（README 写“true / h1 … h5”，默认按 h3 的例子）
const JINA_DEFAULT_HEADING_LEVEL: u8 = 3;
/// [协议] Jina cookbooks.md：`structured` 从 `s3` 开始
const JINA_DEFAULT_STRUCTURED_LEVEL: u8 = 3;

/// 分块选项（交给引擎的 `chunking`）。结构切分的五档对应 Markdown 由粗到细的五层结构：
/// s1 节（各级标题处）、s2 段落（空行；表格、代码块、标题各自成块）、s3 行、s4 句子、s5 子句（标点）。
fn chunk_spec(c: &Chunking) -> Value {
    match c {
        Chunking::Headings(n) => json!({ "type": "headings", "maxLevel": n }),
        Chunking::Structured(1) => json!({ "type": "headings", "maxLevel": 6 }), // [协议] Markdown 的最深标题级别
        Chunking::Structured(2) => json!({ "type": "paragraphs" }),
        Chunking::Structured(3) => json!({ "type": "regex", "patterns": ["\n"] }),
        Chunking::Structured(4) => json!({ "type": "sentences" }),
        Chunking::Structured(_) => {
            json!({ "type": "regex", "patterns": ["[,;:，；：、]\\s*", "\n"] })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn maps_selectors_engine_timeout_cache_and_presets() {
        let (o, p) = parse(&hdrs(&[
            ("x-engine", "curl"),
            ("x-target-selector", "article"),
            ("x-remove-selector", ".ads"),
            ("x-timeout", "25"),
            ("x-no-cache", "true"),
            ("x-preset", "index"),
            ("x-retain-links", "all"),
        ]))
        .unwrap();
        assert_eq!(o.engine.as_deref(), Some("http"));
        assert_eq!(o.include_tags, ["article"]);
        assert_eq!(o.exclude_tags, [".ads"]);
        assert_eq!(o.timeout, Some(25_000));
        assert_eq!(o.respond_timing.as_deref(), Some("network-idle"));
        assert_eq!(o.max_age, Some(0));
        // 预设的 retain-links: text 被调用方的 all 覆盖，其余照预设
        assert_eq!(p.retain_links, Retain::All);
        assert_eq!(p.retain_images, Retain::Text);
        assert_eq!(p.chunking, Some(Chunking::Structured(3)));
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        for h in [
            hdrs(&[("x-with-generated-alt", "true")]),
            hdrs(&[("x-proxy-url", "http://10.0.0.1:3128")]),
            hdrs(&[("x-respond-with", "vlm")]),
            hdrs(&[("x-retain-media", "html")]),
            hdrs(&[("x-markdown-chunking", "h9")]),
            hdrs(&[("x-respond-timing", "eventually")]),
        ] {
            assert!(parse(&h).is_err(), "{h:?}");
        }
        let (o, _) = parse(&hdrs(&[("x-proxy-url", "office")])).unwrap();
        assert_eq!(o.egress.as_deref(), Some("office"));
    }
}
