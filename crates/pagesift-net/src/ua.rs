//! User-Agent：默认值和"不冒充浏览器"校验。

pub const DEFAULT_USER_AGENT: &str = concat!(
    "pagesift/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/YOLOW92/pagesift/blob/main/docs/bot.md)"
);

const BROWSER_MARKERS: [&str; 20] = [
    "mozilla/",
    "applewebkit",
    "like gecko",
    "gecko/",
    "chrome/",
    "chromium/",
    "headlesschrome",
    "crios/",
    "safari/",
    "firefox/",
    "fxios/",
    "edg/",
    "edge/",
    "edga/",
    "opr/",
    "opera",
    "trident/",
    "msie ",
    "samsungbrowser",
    "yabrowser",
];

/// UA 为什么被拒绝。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UserAgentError {
    #[error("user agent is empty")]
    Empty,
    #[error("user agent contains control characters")]
    Control,
    #[error("user agent imitates a browser (`{0}`); identify your bot honestly")]
    Browser(&'static str),
}

/// 拒绝空 UA 和包含浏览器标记的 UA。
pub fn validate_user_agent(ua: &str) -> Result<(), UserAgentError> {
    if ua.trim().is_empty() {
        return Err(UserAgentError::Empty);
    }
    if ua.chars().any(|c| c.is_control()) {
        return Err(UserAgentError::Control);
    }
    let lower = ua.to_ascii_lowercase();
    match BROWSER_MARKERS.iter().find(|m| lower.contains(*m)) {
        Some(m) => Err(UserAgentError::Browser(m)),
        None => Ok(()),
    }
}

/// robots.txt 用的产品标识：UA 里第一个 `/` 或空白之前的部分，如 `pagesift`。
pub fn product_token(ua: &str) -> &str {
    let ua = ua.trim();
    ua.split(['/', ' ', '\t']).next().unwrap_or(ua)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        assert!(
            DEFAULT_USER_AGENT.starts_with(&format!("pagesift/{} ", env!("CARGO_PKG_VERSION")))
        );
        assert!(DEFAULT_USER_AGENT.ends_with("docs/bot.md)"));
        validate_user_agent(DEFAULT_USER_AGENT).unwrap();
        assert_eq!(product_token(DEFAULT_USER_AGENT), "pagesift");
    }

    #[test]
    fn rejects_browser_like() {
        for ua in [
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36",
            "mozilla/5.0 (compatible; MyBot/1.0)",
            "MyBot/1.0 AppleWebKit/537.36",
            "Opera/9.80",
            "MyBot (like Gecko)",
            "",
            "  ",
            "bot\r\nX-Evil: 1",
        ] {
            assert!(validate_user_agent(ua).is_err(), "{ua:?}");
        }
        validate_user_agent("AcmeArchiver/2.1 (+https://acme.example/bot)").unwrap();
    }
}
