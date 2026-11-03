//! 浏览器上下文的代理：`Target.createBrowserContext` 的 `proxyServer`（Chrome DevTools Protocol，
//! Target 域），一个上下文只用一个出口。代理要求认证时由 `Fetch.authRequired` 应答
//! （CDP Fetch 域，`handleAuthRequests`），只回答来源是代理的质询，网站自己的认证一律取消。
//!
//! 只有 Chrome 支持按上下文设置代理；Chrome 不支持 SOCKS 代理的用户名密码认证，这种出口不能用于渲染。
//! 凭据只在内存里，不写进日志。

use url::Url;

/// 解析好的浏览器代理。
#[derive(Clone, PartialEq, Eq)]
pub struct BrowserProxy {
    /// `scheme://host:port`，不含凭据。
    pub server: String,
    /// 代理认证（用户名，密码）。
    pub auth: Option<(String, String)>,
}

impl std::fmt::Debug for BrowserProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserProxy")
            .field("server", &self.server)
            .field("auth", &self.auth.as_ref().map(|_| "***"))
            .finish()
    }
}

impl BrowserProxy {
    /// 解析出口的代理地址；浏览器用不了的（不认识的协议、带认证的 SOCKS）返回错误说明。
    pub fn parse(proxy: &str) -> Result<Self, String> {
        let u = Url::parse(proxy).map_err(|e| format!("invalid proxy URL: {e}"))?;
        let scheme = match u.scheme() {
            "http" | "https" => u.scheme(),
            // Chrome 的 socks5 由代理解析主机名，与 socks5h 相同
            "socks5" | "socks5h" => "socks5",
            other => return Err(format!("browser cannot use {other}:// proxies")),
        };
        let host = u.host_str().ok_or("proxy URL has no host")?;
        let port = u.port_or_known_default().ok_or("proxy URL needs a port")?;
        let auth = (!u.username().is_empty()).then(|| {
            let decode = |s: &str| {
                // 表单解码把 `+` 当空格、按 `&` 分段；URL 的 userinfo 里它们就是字符本身
                let escaped = s.replace('+', "%2B").replace('&', "%26");
                url::form_urlencoded::parse(format!("x={escaped}").as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default()
            };
            (decode(u.username()), decode(u.password().unwrap_or("")))
        });
        if auth.is_some() && scheme == "socks5" {
            return Err("browser cannot authenticate to SOCKS proxies".into());
        }
        Ok(Self {
            server: format!("{scheme}://{host}:{port}"),
            auth,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_parse_into_server_and_credentials() {
        let p = BrowserProxy::parse("http://user:p%40ss@proxy.example:8080").unwrap();
        assert_eq!(p.server, "http://proxy.example:8080");
        assert_eq!(p.auth, Some(("user".into(), "p@ss".into())));
        assert!(!format!("{p:?}").contains("p@ss"), "凭据不进日志");
        let s = BrowserProxy::parse("socks5h://127.0.0.1:1080").unwrap();
        assert_eq!(s.server, "socks5://127.0.0.1:1080");
        assert_eq!(s.auth, None);
        assert!(BrowserProxy::parse("socks5://u:p@127.0.0.1:1080").is_err());
        assert!(BrowserProxy::parse("ftp://h:21").is_err());
        assert_eq!(
            BrowserProxy::parse("http://[::1]:3128").unwrap().server,
            "http://[::1]:3128"
        );
    }
}
