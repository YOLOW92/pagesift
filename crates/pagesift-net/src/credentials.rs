//! 授权凭证（F-1.11）：只发给对应主机，Debug 输出里脱敏。

use std::fmt;

/// 敏感字符串。Debug 和 Display 都不输出内容。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Secret {
    fn from(v: &str) -> Self {
        Self::new(v)
    }
}

impl From<String> for Secret {
    fn from(v: String) -> Self {
        Self(v)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"***\"")
    }
}

/// 管理员按主机配置的凭证。`host` 精确匹配（不含端口，不区分大小写，不匹配子域名）。
#[derive(Debug, Clone, Default)]
pub struct HostCredentials {
    pub host: String,
    pub headers: Vec<(String, Secret)>,
    pub cookies: Vec<(String, Secret)>,
}

impl HostCredentials {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            ..Default::default()
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<Secret>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn cookie(mut self, name: impl Into<String>, value: impl Into<Secret>) -> Self {
        self.cookies.push((name.into(), value.into()));
        self
    }

    pub(crate) fn matches(&self, host: &str) -> bool {
        self.host
            .trim_end_matches('.')
            .eq_ignore_ascii_case(host.trim_end_matches('.'))
    }
}

/// 去掉 URL 里的用户名和密码，用于错误信息和日志。
pub fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            if !u.username().is_empty() || u.password().is_some() {
                let _ = u.set_username("***");
                let _ = u.set_password(None);
            }
            u.to_string()
        }
        Err(_) => "<invalid url>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let c = HostCredentials::new("api.example.com")
            .header("Authorization", "Bearer sk-live-123")
            .cookie("session", "abc");
        let s = format!("{c:?}");
        assert!(!s.contains("sk-live") && !s.contains("abc\""), "{s}");
        assert!(s.contains("Authorization") && s.contains("***"));
    }

    #[test]
    fn redacts_userinfo() {
        assert_eq!(
            redact_url("http://user:pw@proxy.corp:3128/"),
            "http://***@proxy.corp:3128/"
        );
        assert_eq!(redact_url("https://a.com/x"), "https://a.com/x");
    }
}
