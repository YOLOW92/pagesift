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

pub(crate) fn header_links<'a>(base: &Url, fields: impl Iterator<Item = &'a str>) -> Vec<String> {
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
            let mut navigation = false;
            let mut anchored = false;
            for param in split_field(params, ';') {
                let Some((name, value)) = param.trim().split_once('=') else {
                    continue;
                };
                let name = name.trim();
                let value = value.trim();
                // An anchor changes the link context: do not treat it as a
                // navigation edge from the fetched document.
                anchored |= name.eq_ignore_ascii_case("anchor");
                if name.eq_ignore_ascii_case("rel") {
                    let value = value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(value);
                    navigation |= value
                        .split_ascii_whitespace()
                        .any(|r| r.eq_ignore_ascii_case("next") || r.eq_ignore_ascii_case("prev"));
                }
            }
            if !navigation || anchored || target.trim().is_empty() || target.starts_with('#') {
                continue;
            }
            if let Ok(mut url) = base.join(target)
                && matches!(url.scheme(), "http" | "https")
            {
                url.set_fragment(None);
                let url = url.to_string();
                if !out.contains(&url) {
                    out.push(url);
                }
            }
        }
    }
    out
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
}
