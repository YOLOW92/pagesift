//! Bounded static navigation discovery. This is a lexer, not a JavaScript VM:
//! computed strings, expressions, write methods and unknown fetch options abstain.
use std::collections::{HashMap, HashSet};
use url::Url;

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Literal(String),
    Mark(u8),
}
const MAX_SCRIPT: usize = 1024 * 1024;
const MAX_LINKS: usize = 256;
const MAX_TOKENS: usize = 65_536;

/// Static module dependencies and navigation candidates, never execution proof.
#[derive(Debug, Clone, Default)]
pub struct ScriptScan {
    pub navigation: Vec<String>,
    pub dependencies: Vec<String>,
    pub budget_exceeded: bool,
}

/// Opt-in analysis of a standalone script. Resolves simple immutable string
/// constants and concatenations; unknown expressions, writes and shadowed names
/// abstain. Bare module specifiers need an import map and are not guessed.
pub fn scan_script(script: &str, base: &Url) -> ScriptScan {
    if script.len() > MAX_SCRIPT {
        return ScriptScan {
            budget_exceeded: true,
            ..Default::default()
        };
    }
    let tokens = lex(script);
    if tokens.is_empty() && !script.trim().is_empty() {
        return ScriptScan {
            budget_exceeded: true,
            ..Default::default()
        };
    }
    let mut bindings = HashMap::new();
    let mut definitions = HashMap::<&str, usize>::new();
    for window in tokens.windows(2) {
        if let [
            Token::Word("const" | "let" | "var" | "function" | "class"),
            Token::Word(name),
        ] = window
        {
            *definitions.entry(name).or_default() += 1;
        }
    }
    if definitions.len() > 256 {
        return ScriptScan {
            budget_exceeded: true,
            ..Default::default()
        };
    }
    // This bounded folder has no scope tree. With functions/arrow parameters or
    // eval/with present, leave identifiers unresolved instead of guessing which
    // binding a use refers to. Literal navigation remains available.
    let has_dynamic_scope = tokens
        .iter()
        .any(|t| matches!(t, Token::Word("function" | "with" | "eval")))
        || tokens
            .windows(2)
            .any(|w| w == [Token::Mark(b'='), Token::Mark(b'>')]);
    for i in 0..tokens.len().saturating_sub(3) {
        if let [
            Token::Word("const"),
            Token::Word(name),
            Token::Mark(b'='),
            ..,
        ] = &tokens[i..]
            && definitions.get(name) == Some(&1)
            && !has_dynamic_scope
            && let Some((value, end)) = expression(&tokens, i + 3, &bindings)
            && tokens.get(end) == Some(&Token::Mark(b';'))
        {
            // Parameters, assignments and property access using this name
            // invalidate folding; retain only consts with ordinary reads.
            let unsafe_use = tokens.iter().enumerate().any(|(j, t)| {
                *t == Token::Word(name)
                    && j != i + 1
                    && (matches!(tokens.get(j + 1), Some(Token::Mark(b'=' | b'+' | b'-')))
                        && tokens.get(j + 2) == tokens.get(j + 1)
                        || tokens.get(j + 1) == Some(&Token::Mark(b'='))
                        || tokens.get(j.wrapping_sub(1)) == Some(&Token::Mark(b'.')))
            });
            if !unsafe_use && bindings.len() < 256 {
                bindings.insert(*name, value);
            }
        }
    }
    let mut out = ScriptScan::default();
    for i in 0..tokens.len() {
        let prior = &tokens[..i];
        let get = ends(prior, &["fetch", "("]);
        let navigation = ends(prior, &["location", ".", "href", "="])
            || ends(prior, &["location", "="])
            || ["assign", "replace"]
                .iter()
                .any(|m| ends(prior, &["location", ".", m, "("]))
            || ends(prior, &["window", ".", "open", "("]);
        if (get || navigation)
            && let Some((value, end)) = expression(&tokens, i, &bindings)
            && (if get {
                tokens.get(end) == Some(&Token::Mark(b')'))
            } else {
                matches!(
                    tokens.get(end),
                    Some(Token::Mark(b';' | b')' | b',' | b'}')) | None
                )
            })
            && let Some(url) = resolve(base, &value)
        {
            push_unique(&mut out.navigation, url);
        }
        if let Token::Literal(value) = &tokens[i] {
            let module = ends(prior, &["import"])
                || ends(prior, &["import", "("]) && tokens.get(i + 1) == Some(&Token::Mark(b')'))
                || ends(prior, &["from"]);
            if module
                && (value.starts_with('.')
                    || value.starts_with('/')
                    || value.starts_with("https://")
                    || value.starts_with("http://"))
                && let Some(url) = resolve(base, value)
            {
                push_unique(&mut out.dependencies, url);
            }
        }
    }
    out
}

fn expression(
    tokens: &[Token<'_>],
    start: usize,
    bindings: &HashMap<&str, String>,
) -> Option<(String, usize)> {
    let mut end = start;
    let mut value = String::new();
    for _ in 0..16 {
        let part = match tokens.get(end)? {
            Token::Literal(v) => v,
            Token::Word(name) => bindings.get(name)?,
            _ => return None,
        };
        if value.len() + part.len() > 8192 {
            return None;
        }
        value.push_str(part);
        end += 1;
        if tokens.get(end) != Some(&Token::Mark(b'+')) {
            return Some((value, end));
        }
        end += 1;
    }
    None
}
fn resolve(base: &Url, value: &str) -> Option<String> {
    if value.trim().is_empty() || value.starts_with('#') {
        return None;
    }
    let mut u = base.join(value).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some()
    {
        return None;
    }
    u.set_fragment(None);
    Some(u.to_string())
}
fn push_unique(items: &mut Vec<String>, url: String) {
    if items.len() < MAX_LINKS && !items.contains(&url) {
        items.push(url);
    }
}

pub(crate) fn discover(script: &str, base: Option<&Url>) -> Vec<String> {
    if script.len() > MAX_SCRIPT {
        return Vec::new();
    }
    let tokens = lex(script);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (i, token) in tokens.iter().enumerate() {
        let Token::Literal(value) = token else {
            continue;
        };
        let prior = &tokens[..i];
        let navigation = ends(prior, &["location", ".", "href", "="])
            || ends(prior, &["window", ".", "location", "="])
            || ends(prior, &["location", "="])
            || ["assign", "replace"]
                .iter()
                .any(|m| ends(prior, &["location", ".", m, "("]))
            || ends(prior, &["window", ".", "open", "("]);
        let get = ends(prior, &["fetch", "("]) && tokens.get(i + 1) == Some(&Token::Mark(b')'));
        if !navigation && !get {
            continue;
        }
        if !matches!(
            tokens.get(i + 1),
            Some(Token::Mark(b')' | b',' | b';' | b'}')) | None
        ) {
            continue;
        }
        let value = value.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        let url = base
            .and_then(|b| b.join(value).ok())
            .or_else(|| Url::parse(value).ok());
        if let Some(mut url) = url
            && matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
        {
            url.set_fragment(None);
            let url = url.to_string();
            if seen.insert(url.clone()) {
                out.push(url);
            }
            if out.len() >= MAX_LINKS {
                break;
            }
        }
    }
    out
}

fn ends(tokens: &[Token<'_>], pattern: &[&str]) -> bool {
    if tokens.len() < pattern.len() {
        return false;
    }
    let start = tokens.len() - pattern.len();
    // Do not mistake obj.fetch or obj.location for global browser primitives.
    if start > 0
        && matches!(tokens[start - 1], Token::Mark(b'.'))
        && !(start >= 2 && tokens[start - 2] == Token::Word("window"))
    {
        return false;
    }
    tokens[start..].iter().zip(pattern).all(|(t, p)| match t {
        Token::Word(w) => w == p,
        Token::Mark(c) => p.len() == 1 && p.as_bytes()[0] == *c,
        _ => false,
    })
}

fn lex(source: &str) -> Vec<Token<'_>> {
    let b = source.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if out.len() >= MAX_TOKENS {
            return Vec::new();
        }
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            i += 2;
            while i < b.len() && !matches!(b[i], b'\n' | b'\r') {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if matches!(b[i], b'\'' | b'"' | b'`') {
            let quote = b[i];
            i += 1;
            let mut text = Vec::new();
            let mut valid = true;
            let mut closed = false;
            while i < b.len() {
                let c = b[i];
                i += 1;
                if c == quote {
                    closed = true;
                    break;
                }
                if quote == b'`' && c == b'$' && b.get(i) == Some(&b'{') {
                    valid = false;
                }
                if c == b'\\' {
                    let Some(&next) = b.get(i) else {
                        break;
                    };
                    i += 1;
                    match next {
                        b'/' | b'\\' | b'\'' | b'"' | b'`' => text.push(next),
                        b'u' | b'x' => {
                            let len = if next == b'u' { 4 } else { 2 };
                            if let Some(hex) = source
                                .get(i..i + len)
                                .and_then(|v| u32::from_str_radix(v, 16).ok())
                                .and_then(char::from_u32)
                            {
                                let mut buf = [0u8; 4];
                                text.extend_from_slice(hex.encode_utf8(&mut buf).as_bytes());
                                i += len;
                            } else {
                                valid = false;
                            }
                        }
                        _ => valid = false,
                    }
                } else {
                    text.push(c);
                }
            }
            out.push(if valid && closed {
                Token::Literal(String::from_utf8(text).unwrap_or_default())
            } else {
                Token::Mark(b'?')
            });
            continue;
        }
        // Regex literals can contain quoted fake calls. At an expression start,
        // consume the whole regex without interpreting its contents as code.
        if b[i] == b'/'
            && out.last().is_none_or(|t| {
                matches!(
                    t,
                    Token::Mark(b'=' | b'(' | b'[' | b':' | b',' | b'!' | b';')
                ) || *t == Token::Word("return")
            })
        {
            i += 1;
            let mut bracket = false;
            while i < b.len() {
                let c = b[i];
                i += 1;
                if c == b'\\' {
                    i = (i + 1).min(b.len());
                    continue;
                }
                if c == b'[' {
                    bracket = true;
                } else if c == b']' {
                    bracket = false;
                }
                if c == b'/' && !bracket {
                    break;
                }
            }
            out.push(Token::Mark(b'?'));
            continue;
        }
        if b[i].is_ascii_alphabetic() || matches!(b[i], b'_' | b'$') {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$')) {
                i += 1;
            }
            out.push(Token::Word(&source[start..i]));
        } else {
            out.push(Token::Mark(b[i]));
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standalone_modules_fold_only_bounded_unambiguous_constants() {
        let base = Url::parse("https://example.test/assets/app.js").unwrap();
        let scan = scan_script(
            "import './child.js'; const API='/api/'; fetch(API+'list'); fetch('/write',{method:'POST'});",
            &base,
        );
        assert_eq!(scan.navigation, ["https://example.test/api/list"]);
        assert_eq!(scan.dependencies, ["https://example.test/assets/child.js"]);
        for source in [
            "const API='/wrong'; function f(API) {fetch(API);}",
            "const API='/wrong'; (API)=>fetch(API);",
            "const API='/wrong'; API='/different'; fetch(API);",
        ] {
            assert!(scan_script(source, &base).navigation.is_empty());
        }
        let huge = (0..300)
            .map(|i| format!("const X{i}='/x';"))
            .collect::<String>();
        assert!(scan_script(&huge, &base).budget_exceeded);
    }
    #[test]
    fn follows_literal_navigation_and_default_get_only() {
        let base = Url::parse("https://example.test/dir/page").unwrap();
        let code = r#"location.href='/one'; window.location = '../two';
            location.assign("/three#hash"); window.open('/four'); fetch('/five');
            fetch('/write', {method:'POST'}); fetch('/unknown', options);
            fetch('/dynamic' + token); location.href='/computed'+value;
            const unrelated = '/not-a-link'; obj.location.href='/foreign';
            // location.href='/comment';
            /* fetch('/comment-two'); */
            const text = "fetch('/inside-string')";
            const regex = /fetch('\/regex-fake')/;
            fetch(`/template-${value}`); fetch("\u002fescaped");
            fetch('javascript:alert(1)'); fetch('https://user:secret@example.com/private');"#;
        assert_eq!(
            discover(code, Some(&base)),
            [
                "https://example.test/one",
                "https://example.test/two",
                "https://example.test/three",
                "https://example.test/four",
                "https://example.test/five",
                "https://example.test/escaped"
            ]
        );
    }
    #[test]
    fn rejects_oversized_scripts_and_bounds_results() {
        assert!(discover(&"x".repeat(MAX_SCRIPT + 1), None).is_empty());
        assert!(lex(&";".repeat(MAX_TOKENS + 1)).is_empty());
        let js = (0..400)
            .map(|n| format!("fetch('https://example.test/{n}');"))
            .collect::<String>();
        assert_eq!(discover(&js, None).len(), MAX_LINKS);
    }
}
