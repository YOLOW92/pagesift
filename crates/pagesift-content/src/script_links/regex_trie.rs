//! Aho-Corasick 多模式自动机快速静态资源过滤与合法 REST 路由校验。

use aho_corasick::AhoCorasick;
use std::sync::OnceLock;
use url::Url;

/// 静态资产扩展名（不属于导航目标或 REST API 端点）。
pub const STATIC_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".css", ".scss", ".sass", ".less",
    ".woff", ".woff2", ".ttf", ".eot", ".otf", ".map", ".min.map", ".mp4", ".webm", ".ogv", ".mp3",
    ".wav", ".flac", ".pdf", ".zip", ".tar.gz",
];

static ASSET_MATCHER: OnceLock<AhoCorasick> = OnceLock::new();

fn asset_matcher() -> &'static AhoCorasick {
    ASSET_MATCHER.get_or_init(|| {
        AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(STATIC_EXTENSIONS)
            .expect("static asset patterns are valid")
    })
}

/// 判断路径或 URL 是否属于图片、字体、样式等静态资源。
pub fn is_static_asset(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let query_cut = lower.split('?').next().unwrap_or(&lower);
    let hash_cut = query_cut.split('#').next().unwrap_or(query_cut);
    asset_matcher().find(hash_cut).is_some_and(|m| {
        let matched = &hash_cut[m.start()..m.end()];
        hash_cut.ends_with(matched)
    })
}

/// 解析并校验导航/API 目标 URL。
/// 剔除静态资源干扰、非法 scheme、包含用户密码的风险链接，标准化 fragment。
pub fn resolve_navigation_url(base: &Url, value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("javascript:") {
        return None;
    }
    if is_static_asset(trimmed) {
        return None;
    }

    let mut u = base.join(trimmed).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some()
    {
        return None;
    }
    u.set_fragment(None);
    Some(u.to_string())
}

/// 快速去重插入，保证受控预算上限。
pub fn push_bounded_unique(items: &mut Vec<String>, url: String, max_limit: usize) {
    if items.len() < max_limit && !items.contains(&url) {
        items.push(url);
    }
}
