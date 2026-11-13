//! Read-only document navigation from HTTP Link fields. Candidates still pass
//! through the normal crawler scope, robots and budget checks.
use url::Url;

// A comma in a URI-reference or a quoted parameter is not a link separator.
fn split_field(value: &str, delimiter: char) -> Vec<&str> {
    let (mut quoted, mut escaped, mut angle, mut start) = (false, false, false, 0);
    let mut parts = Vec::new();
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if quoted && c == '\\' {
            escaped = true;
        } else if c == '"' && !angle {
            quoted = !quoted;
        } else if !quoted {
            match c {
                '<' => angle = true,
                '>' => angle = false,
                _ if c == delimiter && !angle => {
                    parts.push(&value[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            }
        }
    }
    if !quoted && !angle {
        parts.push(&value[start..]);
    }
    parts
}

struct ParsedLinkItem {
    url: String,
    rels: Vec<String>,
    hreflang: Option<String>,
    media_type: Option<String>,
}

fn parse_link_header<'a>(base: &Url, fields: impl Iterator<Item = &'a str>) -> Vec<ParsedLinkItem> {
    let mut out = Vec::new();
    for field in fields {
        for item in split_field(field, ',') {
            let item = item.trim();
            let Some(rest) = item.strip_prefix('<') else {
                continue;
            };
            let Some((target, params)) = rest.split_once('>') else {
                continue;
            };
            if !params.trim_start().starts_with(';') {
                continue;
            }
            let mut rels = Vec::new();
            let mut hreflang = None;
            let mut media_type = None;
            let mut anchored = false;
            for param in split_field(params, ';') {
                let Some((name, value)) = param.trim().split_once('=') else {
                    continue;
                };
                let name = name.trim();
                let value = value
                    .trim()
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(value.trim());
                // An anchor changes the link context: do not treat it as a
                // navigation or canonical edge from the fetched document.
                anchored |= name.eq_ignore_ascii_case("anchor");
                if name.eq_ignore_ascii_case("rel") {
                    for r in value.split_ascii_whitespace() {
                        rels.push(r.to_ascii_lowercase());
                    }
                } else if name.eq_ignore_ascii_case("hreflang") && !value.is_empty() {
                    hreflang = Some(value.to_string());
                } else if name.eq_ignore_ascii_case("type") && !value.is_empty() {
                    media_type = Some(value.to_string());
                }
            }
            if anchored || target.trim().is_empty() || target.starts_with('#') {
                continue;
            }
            if let Ok(mut url) = base.join(target.trim())
                && matches!(url.scheme(), "http" | "https")
            {
                url.set_fragment(None);
                out.push(ParsedLinkItem {
                    url: url.to_string(),
                    rels,
                    hreflang,
                    media_type,
                });
            }
        }
    }
    out
}

pub fn header_links<'a>(base: &Url, fields: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for item in parse_link_header(base, fields) {
        if item.rels.iter().any(|r| r == "next" || r == "prev") && !out.contains(&item.url) {
            out.push(item.url);
        }
    }
    out
}

/// Extract HTTP `Link: <...>; rel="canonical"` and `rel="alternate"` targets.
pub fn header_canonical_and_alternates<'a>(
    base: &Url,
    fields: impl Iterator<Item = &'a str>,
) -> (Option<String>, Vec<(String, String)>) {
    let mut canonical = None;
    let mut alternates = Vec::new();
    for item in parse_link_header(base, fields) {
        if canonical.is_none() && item.rels.iter().any(|r| r == "canonical") {
            canonical = Some(item.url.clone());
        }
        if item.rels.iter().any(|r| r == "alternate") {
            let label = item
                .hreflang
                .or(item.media_type)
                .unwrap_or_else(|| "alternate".to_string());
            let pair = (label, item.url);
            if !alternates.contains(&pair) {
                alternates.push(pair);
            }
        }
    }
    (canonical, alternates)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_respects_field_syntax_and_context() {
        let base = Url::parse("https://example.com/articles/1").unwrap();
        let fields = [
            r#"<2?a=1,2>; title="a,b;c"; rel="next", </style>; rel=preload"#,
            r#"</other>; rel="prev"; anchor="/different", <javascript:bad>; rel=next"#,
            r#"</previous#part>; REL="PREV", </previous>; rel=prev, </canonical>; rel=canonical"#,
            r#"</bad>; rel="next"; title="unterminated"#,
        ];
        assert_eq!(
            header_links(&base, fields.into_iter()),
            [
                "https://example.com/articles/2?a=1,2",
                "https://example.com/previous"
            ]
        );
    }

    #[test]
    fn canonical_and_alternate_headers_are_extracted() {
        let base = Url::parse("https://example.com/files/spec.pdf").unwrap();
        let fields = [
            r#"</canonical/spec.pdf>; rel="canonical", </es/spec.pdf>; rel="alternate"; hreflang="es""#,
            r#"</spec.json>; rel="alternate"; type="application/json""#,
        ];
        let (canonical, alternates) = header_canonical_and_alternates(&base, fields.into_iter());
        assert_eq!(
            canonical.as_deref(),
            Some("https://example.com/canonical/spec.pdf")
        );
        assert_eq!(
            alternates,
            [
                (
                    "es".to_string(),
                    "https://example.com/es/spec.pdf".to_string()
                ),
                (
                    "application/json".to_string(),
                    "https://example.com/spec.json".to_string()
                ),
            ]
        );
    }
}
