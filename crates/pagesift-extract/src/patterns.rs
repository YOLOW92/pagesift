//! 正则实体提取，对应 Crawl4AI 的 `RegexExtractionStrategy`，内置模式的名称和它一致。

use std::collections::HashMap;
use std::net::Ipv6Addr;
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::Error;

/// 内置模式。`label()` 是输出里的标签，和 Crawl4AI 的 `DEFAULT_PATTERNS` 键名相同；
/// `PhoneCn`、`DateCjk` 是这里新增的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BuiltinPattern {
    Email,
    PhoneIntl,
    PhoneUs,
    PhoneCn,
    Url,
    Ipv4,
    Ipv6,
    Uuid,
    Currency,
    Percentage,
    Number,
    DateIso,
    DateUs,
    DateCjk,
    Time24h,
    PostalUs,
    PostalUk,
    HtmlColorHex,
    TwitterHandle,
    Hashtag,
    MacAddr,
    Iban,
    CreditCard,
}

enum Check {
    None,
    Ipv6,
    Luhn,
    TrimUrl,
}

struct Def {
    label: &'static str,
    pattern: &'static str,
    /// 取哪个捕获组（Rust regex 没有后行断言，用前置分组代替）。
    group: usize,
    check: Check,
}

impl BuiltinPattern {
    pub const ALL: &'static [BuiltinPattern] = &[
        Self::Email,
        Self::PhoneIntl,
        Self::PhoneUs,
        Self::PhoneCn,
        Self::Url,
        Self::Ipv4,
        Self::Ipv6,
        Self::Uuid,
        Self::Currency,
        Self::Percentage,
        Self::Number,
        Self::DateIso,
        Self::DateUs,
        Self::DateCjk,
        Self::Time24h,
        Self::PostalUs,
        Self::PostalUk,
        Self::HtmlColorHex,
        Self::TwitterHandle,
        Self::Hashtag,
        Self::MacAddr,
        Self::Iban,
        Self::CreditCard,
    ];

    pub fn label(self) -> &'static str {
        self.def().label
    }

    /// 按标签查找，例如 `"email"`、`"phone_intl"`。
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.label() == label)
    }

    fn def(self) -> Def {
        let d = |label, pattern, group, check| Def {
            label,
            pattern,
            group,
            check,
        };
        use BuiltinPattern::*;
        match self {
            Email => d("email", r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", 0, Check::None),
            PhoneIntl => d("phone_intl", r"\+?\d[\d .()-]{7,}\d", 0, Check::None),
            PhoneUs => d(
                "phone_us",
                r"\(?\d{3}\)?[-. ]?\d{3}[-. ]?\d{4}",
                0,
                Check::None,
            ),
            PhoneCn => d(
                "phone_cn",
                r"(?-u:\b)1[3-9][0-9]{9}(?-u:\b)",
                0,
                Check::None,
            ),
            Url => d("url", r#"https?://[^\s"'<>]+"#, 0, Check::TrimUrl),
            Ipv4 => d(
                "ipv4",
                r"(?-u:\b)(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?-u:\b)",
                0,
                Check::None,
            ),
            Ipv6 => d(
                "ipv6",
                r"(?:[0-9a-f]{0,4}:){2,7}[0-9a-f]{0,4}",
                0,
                Check::Ipv6,
            ),
            Uuid => d(
                "uuid",
                r"[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
                0,
                Check::None,
            ),
            Currency => d(
                "currency",
                r"(?:(?:USD|EUR|GBP|JPY|CNY|RMB|RM|US\$|HK\$|[$€£¥￥₹₩])\s?(?:[0-9]{1,3}(?:,[0-9]{3})+|[0-9]+)(?:\.[0-9]{1,2})?|(?:[0-9]{1,3}(?:,[0-9]{3})+|[0-9]+)(?:\.[0-9]{1,2})?\s?(?:元|円|美元|欧元|USD|EUR|CNY|RMB))",
                0,
                Check::None,
            ),
            Percentage => d("percentage", r"\d+(?:\.\d+)?\s?[%％]", 0, Check::None),
            Number => d(
                "number",
                r"(?-u:\b)\d{1,3}(?:[,.\s]\d{3})*(?:\.\d+)?(?-u:\b)",
                0,
                Check::None,
            ),
            DateIso => d(
                "date_iso",
                r"(?-u:\b)\d{4}-\d{2}-\d{2}(?-u:\b)",
                0,
                Check::None,
            ),
            DateUs => d(
                "date_us",
                r"(?-u:\b)\d{1,2}/\d{1,2}/\d{2,4}(?-u:\b)",
                0,
                Check::None,
            ),
            DateCjk => d(
                "date_cjk",
                r"\d{4}\s?年\s?\d{1,2}\s?月(?:\s?\d{1,2}\s?[日号])?",
                0,
                Check::None,
            ),
            Time24h => d(
                "time_24h",
                r"(?-u:\b)(?:[01]?\d|2[0-3]):[0-5]\d(?:[:.][0-5]\d)?(?-u:\b)",
                0,
                Check::None,
            ),
            PostalUs => d(
                "postal_us",
                r"(?-u:\b)\d{5}(?:-\d{4})?(?-u:\b)",
                0,
                Check::None,
            ),
            PostalUk => d(
                "postal_uk",
                r"(?-u:\b)[A-Z]{1,2}\d[A-Z\d]? ?\d[A-Z]{2}(?-u:\b)",
                0,
                Check::None,
            ),
            HtmlColorHex => d(
                "html_color_hex",
                r"#(?:[0-9A-F]{6}|[0-9A-F]{3})(?-u:\b)",
                0,
                Check::None,
            ),
            TwitterHandle => d(
                "twitter_handle",
                r"(?:^|[^\w@.])(@\w{1,15})(?-u:\b)",
                1,
                Check::None,
            ),
            Hashtag => d("hashtag", r"(?:^|[^\w&#/])(#[\w-]+)", 1, Check::None),
            MacAddr => d(
                "mac_addr",
                r"(?-u:\b)(?:[0-9A-F]{2}:){5}[0-9A-F]{2}(?-u:\b)",
                0,
                Check::None,
            ),
            Iban => d(
                "iban",
                r"(?-u:\b)[A-Z]{2}\d{2}[A-Z0-9]{11,30}(?-u:\b)",
                0,
                Check::None,
            ),
            CreditCard => d(
                "credit_card",
                r"(?-u:\b)(?:4\d{12}(?:\d{3})?|5[1-5]\d{14}|3[47]\d{13}|6(?:011|5\d{2})\d{12})(?-u:\b)",
                0,
                Check::Luhn,
            ),
        }
    }
}

struct Compiled {
    label: String,
    re: Regex,
    group: usize,
    check: Check,
}

/// 一次匹配。`span` 是字符（Unicode 标量）下标，和 Crawl4AI（Python）一致；`byte_span` 是 UTF-8 字节下标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegexMatch {
    pub label: String,
    pub value: String,
    pub span: [usize; 2],
    pub byte_span: [usize; 2],
}

/// 去重后的值，按首次出现顺序排列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UniqueMatch {
    pub label: String,
    pub value: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RegexOutput {
    /// 按模式顺序分组，组内按位置排序（与 Crawl4AI 的输出顺序一致）。
    pub matches: Vec<RegexMatch>,
    pub unique: Vec<UniqueMatch>,
}

/// 编译好的一组模式，可以跨线程复用。和 Crawl4AI 一样按忽略大小写、多行模式编译。
pub struct RegexExtractor {
    patterns: Vec<Compiled>,
}

impl RegexExtractor {
    /// `custom` 是 `(标签, 正则)`；标签和内置模式同名时覆盖内置模式。
    pub fn new<L: AsRef<str>, P: AsRef<str>>(
        builtins: &[BuiltinPattern],
        custom: &[(L, P)],
    ) -> Result<Self, Error> {
        let mut patterns: Vec<Compiled> = Vec::new();
        for b in builtins {
            let d = b.def();
            if patterns.iter().any(|p| p.label == d.label) {
                continue;
            }
            patterns.push(Compiled {
                label: d.label.to_owned(),
                re: builtin_regex(*b),
                group: d.group,
                check: d.check,
            });
        }
        for (label, pattern) in custom {
            let (label, pattern) = (label.as_ref(), pattern.as_ref());
            let re = build(pattern).map_err(|e| Error::Regex {
                path: format!("custom.{label}"),
                message: format!("{e} (note: look-around and backreferences are not supported)"),
            })?;
            let c = Compiled {
                label: label.to_owned(),
                re,
                group: 0,
                check: Check::None,
            };
            match patterns.iter_mut().find(|p| p.label == label) {
                Some(slot) => *slot = c,
                None => patterns.push(c),
            }
        }
        Ok(RegexExtractor { patterns })
    }

    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.patterns.iter().map(|p| p.label.as_str())
    }

    pub fn extract(&self, text: &str) -> RegexOutput {
        let mut out = RegexOutput::default();
        let mut seen: HashMap<(usize, &str), usize> = HashMap::new();
        for (pi, p) in self.patterns.iter().enumerate() {
            let mut chars = CharIndex::new(text);
            for caps in p.re.captures_iter(text) {
                let Some(m) = caps.get(p.group) else { continue };
                let (start, mut end) = (m.start(), m.end());
                match p.check {
                    Check::None => {}
                    Check::Ipv6 => {
                        if m.as_str().parse::<Ipv6Addr>().is_err() {
                            continue;
                        }
                    }
                    Check::Luhn => {
                        if !luhn(m.as_str()) {
                            continue;
                        }
                    }
                    Check::TrimUrl => end = start + trim_url(m.as_str()).len(),
                }
                let value = &text[start..end];
                if value.is_empty() {
                    continue;
                }
                match seen.get(&(pi, value)) {
                    Some(&i) => out.unique[i].count += 1,
                    None => {
                        seen.insert((pi, value), out.unique.len());
                        out.unique.push(UniqueMatch {
                            label: p.label.clone(),
                            value: value.to_owned(),
                            count: 1,
                        });
                    }
                }
                out.matches.push(RegexMatch {
                    label: p.label.clone(),
                    value: value.to_owned(),
                    span: [chars.at(start), chars.at(end)],
                    byte_span: [start, end],
                });
            }
        }
        out
    }
}

/// 内置模式只编译一次，`Regex` 克隆很便宜。
fn builtin_regex(p: BuiltinPattern) -> Regex {
    static CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
    let all = CACHE.get_or_init(|| {
        BuiltinPattern::ALL
            .iter()
            .map(|b| build(b.def().pattern).expect("built-in patterns compile"))
            .collect()
    });
    let i = BuiltinPattern::ALL
        .iter()
        .position(|b| *b == p)
        .unwrap_or(0);
    all[i].clone()
}

fn build(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .multi_line(true)
        .build()
}

/// 字节下标转字符下标；查询的下标单调递增时是线性的。
struct CharIndex<'a> {
    text: &'a str,
    ascii: bool,
    byte: usize,
    chars: usize,
}

impl<'a> CharIndex<'a> {
    fn new(text: &'a str) -> Self {
        CharIndex {
            text,
            ascii: text.is_ascii(),
            byte: 0,
            chars: 0,
        }
    }

    fn at(&mut self, byte: usize) -> usize {
        if self.ascii {
            return byte;
        }
        if byte < self.byte {
            self.byte = 0;
            self.chars = 0;
        }
        self.chars += self.text[self.byte..byte].chars().count();
        self.byte = byte;
        self.chars
    }
}

/// 去掉 URL 末尾的标点和不配对的右括号（Markdown 链接、句末标点）。
fn trim_url(url: &str) -> &str {
    let mut s = url;
    loop {
        let Some(last) = s.chars().last() else {
            return s;
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            ']' => s.matches('[').count() < s.matches(']').count(),
            _ => false,
        };
        if !strip {
            return s;
        }
        s = &s[..s.len() - last.len_utf8()];
    }
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.bytes().rev().enumerate() {
        let mut d = u32::from(c - b'0');
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(p: BuiltinPattern, text: &str) -> Vec<String> {
        RegexExtractor::new::<&str, &str>(&[p], &[])
            .unwrap()
            .extract(text)
            .matches
            .into_iter()
            .map(|m| m.value)
            .collect()
    }

    #[test]
    fn builtins() {
        use BuiltinPattern::*;
        assert_eq!(
            values(Email, "Mail a.b+c@ex-ample.co.uk."),
            ["a.b+c@ex-ample.co.uk"]
        );
        assert_eq!(
            values(Url, "see (https://x.io/a_(b)?q=1). and [t](https://y.io/p)"),
            ["https://x.io/a_(b)?q=1", "https://y.io/p"]
        );
        assert_eq!(values(Ipv4, "10.0.0.1 and 999.1.1.1"), ["10.0.0.1"]);
        assert_eq!(
            values(Ipv6, "::1, fe80::1ff:fe23:4567:890a, 12:30:45"),
            ["::1", "fe80::1ff:fe23:4567:890a"]
        );
        assert_eq!(
            values(Uuid, "id 123e4567-e89b-12d3-a456-426614174000"),
            ["123e4567-e89b-12d3-a456-426614174000"]
        );
        assert_eq!(
            values(Currency, "$1,299.99 or ¥ 88 or 25元"),
            ["$1,299.99", "¥ 88", "25元"]
        );
        assert_eq!(values(Percentage, "up 12.5% and 3％"), ["12.5%", "3％"]);
        assert_eq!(values(DateIso, "on 2024-05-01."), ["2024-05-01"]);
        assert_eq!(values(DateUs, "on 5/1/2024"), ["5/1/2024"]);
        assert_eq!(values(DateCjk, "发布于2024年5月1日"), ["2024年5月1日"]);
        assert_eq!(
            values(Time24h, "at 23:59 and 7:05:30"),
            ["23:59", "7:05:30"]
        );
        assert_eq!(values(PostalUs, "NY 10001-1234"), ["10001-1234"]);
        assert_eq!(values(PostalUk, "London SW1A 1AA"), ["SW1A 1AA"]);
        assert_eq!(
            values(HtmlColorHex, "color: #FF00aa; #fff"),
            ["#FF00aa", "#fff"]
        );
        assert_eq!(
            values(TwitterHandle, "@rustlang and me@mail.com"),
            ["@rustlang"]
        );
        assert_eq!(
            values(Hashtag, "#rust #中文 and a.com/#frag"),
            ["#rust", "#中文"]
        );
        assert_eq!(values(MacAddr, "00:1A:2B:3C:4D:5E"), ["00:1A:2B:3C:4D:5E"]);
        assert_eq!(
            values(Iban, "GB82WEST12345698765432"),
            ["GB82WEST12345698765432"]
        );
        assert_eq!(
            values(CreditCard, "4111111111111111 4111111111111112"),
            ["4111111111111111"]
        );
        assert_eq!(values(PhoneCn, "电话 13812345678"), ["13812345678"]);
        assert_eq!(values(PhoneUs, "(555) 123-4567"), ["(555) 123-4567"]);
        assert!(!values(PhoneIntl, "+1 555 123 4567").is_empty());
        assert_eq!(values(Number, "1,234,567.89"), ["1,234,567.89"]);
    }

    #[test]
    fn spans_dedup_and_custom() {
        let ex = RegexExtractor::new(
            &[BuiltinPattern::Email],
            &[("sku", r"SKU-\d+"), ("email", r"\w+@corp\.com")],
        )
        .unwrap();
        assert_eq!(ex.labels().collect::<Vec<_>>(), ["email", "sku"]);
        let text = "联系 bob@corp.com sku-1 SKU-1 x@y.com";
        let out = ex.extract(text);
        assert_eq!(out.matches[0].value, "bob@corp.com");
        assert_eq!(out.matches[0].span, [3, 15]);
        assert_eq!(out.matches[0].byte_span, [7, 19]);
        assert_eq!(out.matches.len(), 3);
        assert_eq!(
            out.unique[1],
            UniqueMatch {
                label: "sku".into(),
                value: "sku-1".into(),
                count: 1
            }
        );
        assert_eq!(out.unique.len(), 3);
        let dup = RegexExtractor::new(&[], &[("w", "a")])
            .unwrap()
            .extract("a a a");
        assert_eq!(dup.unique[0].count, 3);
        assert!(RegexExtractor::new(&[], &[("bad", "(?=x)")]).is_err());
    }

    #[test]
    fn labels_roundtrip() {
        for p in BuiltinPattern::ALL {
            assert_eq!(BuiltinPattern::from_label(p.label()), Some(*p));
        }
    }
}
