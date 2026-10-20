//! robots.txt 解析和匹配（RFC 9309）。

use std::time::Duration;

/// RFC 9309 要求至少解析 500 KiB。
pub const MAX_ROBOTS_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    allow: bool,
    /// 规范化后的模式，可含 `*` 和结尾的 `$`。
    pattern: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct Group {
    agents: Vec<String>,
    rules: Vec<Rule>,
    crawl_delay: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Parsed,
    AllowAll,
    DisallowAll,
}

#[derive(Debug, Clone)]
pub struct Robots {
    mode: Mode,
    groups: Vec<Group>,
    sitemaps: Vec<String>,
}

/// 某个 UA 适用的规则集合。
#[derive(Debug, Clone)]
pub struct RobotsMatcher<'a> {
    mode: Mode,
    rules: Vec<&'a Rule>,
    crawl_delay: Option<Duration>,
}

impl Robots {
    /// 4xx 时使用：全部允许。
    pub fn allow_all() -> Self {
        Self {
            mode: Mode::AllowAll,
            groups: Vec::new(),
            sitemaps: Vec::new(),
        }
    }

    /// 5xx 或无法访问时使用：全部禁止。
    pub fn disallow_all() -> Self {
        Self {
            mode: Mode::DisallowAll,
            ..Self::allow_all()
        }
    }

    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let text = truncate(text, MAX_ROBOTS_BYTES);
        let mut groups: Vec<Group> = Vec::new();
        let mut sitemaps = Vec::new();
        // 上一行是否 user-agent：连续的 user-agent 行属于同一组。
        let mut in_agents = false;
        for line in text.split(['\n', '\r']) {
            let line = line.split('#').next().unwrap_or("");
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "user-agent" | "useragent" | "user agent" => {
                    if !in_agents {
                        groups.push(Group::default());
                    }
                    in_agents = true;
                    if let Some(g) = groups.last_mut() {
                        g.agents.push(agent_token(value).to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    in_agents = false;
                    let Some(g) = groups.last_mut() else { continue };
                    // 空的 Disallow 表示不禁止任何路径。
                    if value.is_empty() {
                        continue;
                    }
                    let mut pattern = normalize(value.as_bytes());
                    if !matches!(pattern.first(), Some(b'/' | b'*')) {
                        pattern.insert(0, b'/');
                    }
                    g.rules.push(Rule {
                        allow: key == "allow",
                        pattern,
                    });
                }
                "crawl-delay" | "crawl_delay" => {
                    in_agents = false;
                    let Some(g) = groups.last_mut() else { continue };
                    if let Ok(secs) = value.parse::<f64>()
                        && secs.is_finite()
                        && secs >= 0.0
                    {
                        g.crawl_delay = Some(Duration::from_secs_f64(secs.min(86_400.0)));
                    }
                }
                "sitemap" | "site-map" if !value.is_empty() => {
                    sitemaps.push(value.to_string());
                }
                _ => {}
            }
        }
        Self {
            mode: Mode::Parsed,
            groups,
            sitemaps,
        }
    }

    pub fn sitemaps(&self) -> &[String] {
        &self.sitemaps
    }

    /// 按产品标识（如 `pagesift`，不区分大小写）选出适用的组；没有匹配的组时用 `*` 组。
    pub fn matcher(&self, product_token: &str) -> RobotsMatcher<'_> {
        let token = product_token.to_ascii_lowercase();
        let pick = |want: &str| -> Vec<&Group> {
            self.groups
                .iter()
                .filter(|g| g.agents.iter().any(|a| a == want))
                .collect()
        };
        let mut chosen = pick(&token);
        if chosen.is_empty() {
            chosen = pick("*");
        }
        RobotsMatcher {
            mode: self.mode,
            rules: chosen.iter().flat_map(|g| g.rules.iter()).collect(),
            // 多个组都写了 Crawl-delay 时取最长的。
            crawl_delay: chosen.iter().filter_map(|g| g.crawl_delay).max(),
        }
    }

    pub fn is_allowed(&self, product_token: &str, path_and_query: &str) -> bool {
        self.matcher(product_token).is_allowed(path_and_query)
    }

    pub fn crawl_delay(&self, product_token: &str) -> Option<Duration> {
        self.matcher(product_token).crawl_delay
    }
}

impl RobotsMatcher<'_> {
    /// `path_and_query` 是 URL 的路径加查询串，如 `/a/b?c=d`。
    pub fn is_allowed(&self, path_and_query: &str) -> bool {
        let path = normalize(path_and_query.as_bytes());
        if path == b"/robots.txt" {
            return true;
        }
        match self.mode {
            Mode::AllowAll => return true,
            Mode::DisallowAll => return false,
            Mode::Parsed => {}
        }
        // 最长匹配优先；长度相同时 Allow 优先。
        let mut best: Option<(usize, bool)> = None;
        for r in &self.rules {
            if matches(&r.pattern, &path) {
                let cand = (r.pattern.len(), r.allow);
                if best.is_none_or(|b| cand > b) {
                    best = Some(cand);
                }
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }

    pub fn crawl_delay(&self) -> Option<Duration> {
        self.crawl_delay
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `pagesift/1.0` 里的 `pagesift`。
fn agent_token(value: &str) -> &str {
    value.split(['/', ' ', '\t']).next().unwrap_or(value)
}

/// RFC 9309 2.2.2：解码非保留字符的百分号编码，其余编码统一成大写，非 ASCII 字节编码。
fn normalize(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'%'
            && i + 2 < s.len()
            && let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2]))
        {
            let c = h * 16 + l;
            if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
                out.push(c);
            } else {
                push_pct(&mut out, c);
            }
            i += 3;
            continue;
        }
        if b >= 0x80 || b <= 0x20 {
            push_pct(&mut out, b);
        } else {
            out.push(b);
        }
        i += 1;
    }
    out
}

fn push_pct(out: &mut Vec<u8>, b: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.extend_from_slice(&[b'%', HEX[usize::from(b >> 4)], HEX[usize::from(b & 15)]]);
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// `*` 匹配任意字符序列，结尾的 `$` 表示必须匹配到末尾；否则是前缀匹配。
fn matches(pattern: &[u8], path: &[u8]) -> bool {
    let (pat, anchored) = match pattern.strip_suffix(b"$") {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let (mut p, mut s) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if p == pat.len() {
            if !anchored || s == path.len() {
                return true;
            }
        } else if pat[p] == b'*' {
            star = Some((p, s));
            p += 1;
            continue;
        } else if s < path.len() && pat[p] == path[s] {
            p += 1;
            s += 1;
            continue;
        }
        // 回溯到上一个 `*`，让它多吞一个字符。
        match star {
            Some((sp, ss)) if ss < path.len() => {
                star = Some((sp, ss + 1));
                p = sp + 1;
                s = ss + 1;
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UA: &str = "pagesift";

    /// (robots.txt, 路径, 是否允许)
    fn table(robots: &str, cases: &[(&str, bool)]) {
        let r = Robots::parse(robots);
        for (path, want) in cases {
            assert_eq!(r.is_allowed(UA, path), *want, "{path} in\n{robots}");
        }
    }

    #[test]
    fn rfc_9309_example() {
        // RFC 9309 第 5.1 节的示例。
        let robots = "\
User-Agent: *
Disallow: *.gif$
Disallow: /example/
Allow: /publications/

User-Agent: foobot
Disallow:/
Allow:/example/page.html
Allow:/example/allowed.gif

User-Agent: barbot
User-Agent: bazbot
Disallow: /example/page.html

User-Agent: quxbot
";
        let r = Robots::parse(robots);
        let cases: &[(&str, &str, bool)] = &[
            ("foobot", "/", false),
            ("foobot", "/example/page.html", true),
            ("foobot", "/example/allowed.gif", true),
            ("foobot", "/other", false),
            ("barbot", "/example/page.html", false),
            ("bazbot", "/example/page.html", false),
            ("barbot", "/example/other.html", true),
            ("quxbot", "/anything", true),
            ("otherbot", "/example/x", false),
            ("otherbot", "/a/b.gif", false),
            ("otherbot", "/a/b.gif?x", true),
            ("otherbot", "/publications/", true),
            ("FooBot", "/", false),
        ];
        for (ua, path, want) in cases {
            assert_eq!(r.is_allowed(ua, path), *want, "{ua} {path}");
        }
    }

    #[test]
    fn longest_match_and_allow_tie() {
        table(
            "user-agent: *\nallow: /p\ndisallow: /\n",
            &[("/page", true), ("/", false), ("/other", false)],
        );
        table(
            "user-agent: *\nallow: /folder\ndisallow: /folder\n",
            &[("/folder/page", true)],
        );
        table(
            "user-agent: *\nallow: /page\ndisallow: /*.html\n",
            &[("/page.html", false), ("/page", true)],
        );
        table(
            "user-agent: *\nallow: /$\ndisallow: /\n",
            &[("/", true), ("/page.html", false)],
        );
    }

    #[test]
    fn wildcards_and_end_anchor() {
        let robots =
            "user-agent: *\ndisallow: /fish*.php\ndisallow: /*/private/\ndisallow: /exact$\n";
        table(
            robots,
            &[
                ("/fish.php", false),
                ("/fishheads/catfish.php?parameters", false),
                ("/Fish.PHP", true),
                ("/a/b/private/c", false),
                ("/private/", true),
                ("/exact", false),
                ("/exact/more", true),
                ("/exactly", true),
            ],
        );
        table(
            "user-agent: *\ndisallow: /a$b\n",
            &[("/a$b", false), ("/a", true)],
        );
        table(
            "user-agent: *\ndisallow: /***x**\n",
            &[("/abcx", false), ("/abc", true)],
        );
    }

    #[test]
    fn percent_encoding_is_normalized() {
        table(
            "user-agent: *\ndisallow: /foo/bar?baz=quz\ndisallow: /%7Euser\ndisallow: /a%2fb\ndisallow: /中文\n",
            &[
                ("/foo/bar?baz=quz", false),
                ("/~user/x", false),
                ("/%7euser", false),
                ("/a%2Fb", false),
                ("/a/b", true),
                ("/%E4%B8%AD%E6%96%87/page", false),
            ],
        );
    }

    #[test]
    fn groups_merge_and_fallback() {
        let robots = "\
user-agent: pagesift
disallow: /a

user-agent: *
disallow: /

user-agent: PageSift/2.0
disallow: /b
";
        table(robots, &[("/a", false), ("/b", false), ("/c", true)]);
        // 没有匹配组也没有 * 组：全部允许。
        table("user-agent: other\ndisallow: /\n", &[("/", true)]);
        // 规则写在任何 user-agent 之前：忽略。
        table("disallow: /\nuser-agent: *\nallow: /\n", &[("/x", true)]);
    }

    #[test]
    fn misc_syntax() {
        table(
            "\u{feff}User-Agent : *   # comment\r\nDisallow : /secret # tail\r\nDisallow:\r\nfoo: bar\n",
            &[("/secret/x", false), ("/public", true)],
        );
        table("user-agent: *\ndisallow: tmp\n", &[("/tmp/x", false)]);
        assert!(Robots::parse("").is_allowed(UA, "/"));
        assert!(Robots::parse("<html>not robots</html>").is_allowed(UA, "/"));
    }

    #[test]
    fn robots_txt_itself_always_allowed() {
        assert!(Robots::disallow_all().is_allowed(UA, "/robots.txt"));
        assert!(!Robots::disallow_all().is_allowed(UA, "/"));
        assert!(Robots::allow_all().is_allowed(UA, "/anything"));
        table("user-agent: *\ndisallow: /\n", &[("/robots.txt", true)]);
    }

    #[test]
    fn crawl_delay_and_sitemaps() {
        let r = Robots::parse(
            "Sitemap: https://a.com/s1.xml\nuser-agent: *\ncrawl-delay: 2.5\nuser-agent: pagesift\ncrawl-delay: 7\n\nuser-agent: x\ncrawl-delay: abc\nsitemap: https://a.com/s2.xml\n",
        );
        assert_eq!(r.crawl_delay(UA), Some(Duration::from_secs(7)));
        assert_eq!(r.crawl_delay("x"), None);
        assert_eq!(
            r.sitemaps(),
            ["https://a.com/s1.xml", "https://a.com/s2.xml"]
        );
        let r = Robots::parse("user-agent: *\ncrawl-delay: 2.5\n");
        assert_eq!(r.crawl_delay(UA), Some(Duration::from_millis(2500)));
    }

    #[test]
    fn huge_input_is_truncated_not_rejected() {
        let mut s = String::from("user-agent: *\ndisallow: /a\n");
        s.push_str(&"# padding\n".repeat(MAX_ROBOTS_BYTES / 10 + 10));
        s.push_str("disallow: /b\n");
        let r = Robots::parse(&s);
        assert!(!r.is_allowed(UA, "/a"));
        assert!(r.is_allowed(UA, "/b"));
    }
}
