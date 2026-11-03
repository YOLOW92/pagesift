//! Cookie 的解析、校验与匹配（RFC 6265 §5.2–§5.4 的子集，按精确源收紧）。

use std::time::UNIX_EPOCH;

use url::Url;

use crate::Secret;

pub(super) struct Cookie {
    pub(super) name: String,
    pub(super) value: Secret,
    pub(super) path: String,
    pub(super) secure: bool,
    pub(super) http_only: bool,
    pub(super) expires_at: Option<u64>,
    pub(super) order: u64,
}
impl Cookie {
    pub(super) fn size(&self) -> usize {
        self.name.len() + self.value.expose().len() + self.path.len() + 64
    }
}
pub(super) fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
pub(super) fn valid_value(s: &str) -> bool {
    s.bytes()
        .all(|b| matches!(b,0x21|0x23..=0x2b|0x2d..=0x3a|0x3c..=0x5b|0x5d..=0x7e))
}
pub(super) fn valid_path(s: &str) -> bool {
    s.starts_with('/') && s.bytes().all(|b| (0x20..=0x7e).contains(&b) && b != b';')
}
pub(super) fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/'))
}
pub(super) fn parse_cookie(
    url: &Url,
    host: &str,
    raw: &str,
    time: u64,
    session_end: u64,
) -> Option<Cookie> {
    if !raw.is_ascii() || raw.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let mut parts = raw.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    if !valid_name(name) || !valid_value(value) {
        return None;
    }
    let default_path = url
        .path()
        .rsplit_once('/')
        .map_or("/", |(p, _)| if p.is_empty() { "/" } else { p });
    let mut path = default_path;
    let mut explicit_root_path = false;
    let (mut secure, mut http_only, mut domain) = (false, false, false);
    let (mut expires, mut max_age) = (None, None);
    for attr in parts {
        let (key, value) = attr.trim().split_once('=').unwrap_or((attr.trim(), ""));
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "domain" => {
                domain = true;
                if !value.trim_start_matches('.').eq_ignore_ascii_case(host) {
                    return None;
                }
            }
            "path" => {
                explicit_root_path = value == "/";
                path = if valid_path(value) {
                    value
                } else {
                    default_path
                };
            }
            "secure" => secure = true,
            "httponly" => http_only = true,
            "max-age" => {
                let digits = value.strip_prefix('-').unwrap_or(value);
                if !digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && let Ok(v) = value.parse::<i64>()
                {
                    max_age = Some(v);
                }
            }
            "expires" => {
                if let Ok(t) = httpdate::parse_http_date(value) {
                    expires = Some(t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs());
                }
            }
            _ => (),
        }
    }
    if secure && url.scheme() != "https"
        || name.starts_with("__Secure-") && (!secure || url.scheme() != "https")
        || name.starts_with("__Host-")
            && (!secure || url.scheme() != "https" || domain || path != "/" || !explicit_root_path)
    {
        return None;
    }
    let expires_at = max_age
        .map(|v| {
            if v <= 0 {
                time
            } else {
                time.saturating_add(v as u64)
            }
        })
        .or(expires)
        .map(|t| t.min(session_end));
    Some(Cookie {
        name: name.into(),
        value: Secret::new(value),
        path: path.into(),
        secure,
        http_only,
        expires_at,
        order: 0,
    })
}
