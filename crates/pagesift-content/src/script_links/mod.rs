//! Bounded static navigation discovery and ECMAScript endpoint extraction.
//! Pure Rust streaming lexer without V8/SWC runtime overhead.

pub mod api_calls;
pub mod ast_scanner;
pub mod regex_trie;
pub mod template_literals;

use std::collections::{HashMap, HashSet};
use url::Url;

use self::api_calls::{ends, expression, parse_helper};
use self::ast_scanner::{MAX_SCRIPT, Token, lex};
use self::regex_trie::{push_bounded_unique, resolve_navigation_url};

pub const MAX_LINKS: usize = 256;

/// Static module dependencies and navigation candidates, never execution proof.
#[derive(Debug, Clone, Default)]
pub struct ScriptScan {
    pub navigation: Vec<String>,
    pub dependencies: Vec<String>,
    pub budget_exceeded: bool,
}

/// Opt-in analysis of a standalone script. Resolves simple immutable string
/// constants, concatenations, template literals and pure string-concatenation helpers;
/// unknown expressions, writes and shadowed names abstain.
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
    let mut helpers = HashMap::new();
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
    let has_eval_or_with = tokens
        .iter()
        .any(|t| matches!(t, Token::Word("with" | "eval")));
    let param_names = collect_param_names(&tokens);
    let unsafe_use = |name: &str, def_idx: usize| {
        tokens.iter().enumerate().any(|(j, t)| {
            *t == Token::Word(name)
                && j != def_idx
                && (matches!(tokens.get(j + 1), Some(Token::Mark(b'=' | b'+' | b'-')))
                    && tokens.get(j + 2) == tokens.get(j + 1)
                    || tokens.get(j + 1) == Some(&Token::Mark(b'='))
                    || tokens.get(j.wrapping_sub(1)) == Some(&Token::Mark(b'.')))
        })
    };
    let mut brace_depth = 0usize;
    for i in 0..tokens.len() {
        match tokens[i] {
            Token::Mark(b'{') => brace_depth += 1,
            Token::Mark(b'}') => brace_depth = brace_depth.saturating_sub(1),
            _ => {}
        }
        if brace_depth != 0 || has_eval_or_with {
            continue;
        }
        if let [
            Token::Word("const"),
            Token::Word(name),
            Token::Mark(b'='),
            ..,
        ] = &tokens[i..]
            && definitions.get(name) == Some(&1)
            && !param_names.contains(name)
            && !unsafe_use(name, i + 1)
        {
            if let Some((value, end)) = expression(&tokens, i + 3, &bindings, &helpers, 0)
                && tokens.get(end) == Some(&Token::Mark(b';'))
            {
                if bindings.len() < 256 {
                    bindings.insert(*name, value);
                }
            } else if let Some(helper) = parse_helper(&tokens, i + 3, true, &bindings)
                && helpers.len() < 64
            {
                helpers.insert(*name, helper);
            }
        } else if let [Token::Word("function"), Token::Word(name), ..] = &tokens[i..]
            && definitions.get(name) == Some(&1)
            && !param_names.contains(name)
            && !unsafe_use(name, i + 1)
            && let Some(helper) = parse_helper(&tokens, i + 2, false, &bindings)
            && helpers.len() < 64
        {
            helpers.insert(*name, helper);
        }
    }
    let mut out = ScriptScan::default();
    for i in 0..tokens.len() {
        let prior = &tokens[..i];
        let get = ends(prior, &["fetch", "("])
            || ends(prior, &["axios", ".", "get", "("])
            || ends(prior, &["$", ".", "get", "("]);
        let navigation = ends(prior, &["location", ".", "href", "="])
            || ends(prior, &["location", "="])
            || ["assign", "replace"]
                .iter()
                .any(|m| ends(prior, &["location", ".", m, "("]))
            || ends(prior, &["window", ".", "open", "("]);
        if (get || navigation)
            && let Some((value, end)) = expression(&tokens, i, &bindings, &helpers, 0)
            && (if get {
                tokens.get(end) == Some(&Token::Mark(b')'))
            } else {
                matches!(
                    tokens.get(end),
                    Some(Token::Mark(b';' | b')' | b',' | b'}')) | None
                )
            })
            && let Some(url) = resolve_navigation_url(base, &value)
        {
            push_bounded_unique(&mut out.navigation, url, MAX_LINKS);
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
                && let Some(url) = resolve_navigation_url(base, value)
            {
                push_bounded_unique(&mut out.dependencies, url, MAX_LINKS);
            }
        }
    }
    out
}

fn collect_param_names<'a>(tokens: &[Token<'a>]) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    for i in 0..tokens.len() {
        if tokens[i] == Token::Word("function") {
            let mut p = i + 1;
            if matches!(tokens.get(p), Some(Token::Word(_))) {
                p += 1;
            }
            if tokens.get(p) == Some(&Token::Mark(b'(')) {
                for t in &tokens[p + 1..] {
                    match t {
                        Token::Mark(b')') => break,
                        Token::Word(w) => {
                            out.insert(*w);
                        }
                        _ => {}
                    }
                }
            }
        }
        if tokens.get(i..i + 2) == Some(&[Token::Mark(b'='), Token::Mark(b'>')]) && i > 0 {
            if let Token::Word(w) = tokens[i - 1] {
                out.insert(w);
            } else if tokens[i - 1] == Token::Mark(b')') {
                for t in tokens[..i - 1].iter().rev() {
                    match t {
                        Token::Mark(b'(') => break,
                        Token::Word(w) => {
                            out.insert(*w);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn discover(script: &str, base: Option<&Url>) -> Vec<String> {
    if script.len() > MAX_SCRIPT {
        return Vec::new();
    }
    let tokens = lex(script);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (i, token) in tokens.iter().enumerate() {
        let value = match token {
            Token::Literal(v) => v.as_str(),
            Token::Template(tmpl) => {
                // 如果是纯静态模板，或者带占位符的路径
                if tmpl.is_static() {
                    tmpl.parts
                        .first()
                        .and_then(|p| match p {
                            template_literals::TemplatePart::Literal(s) => Some(s.as_str()),
                            _ => None,
                        })
                        .unwrap_or("")
                } else {
                    ""
                }
            }
            _ => continue,
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

#[cfg(test)]
mod tests {
    use self::ast_scanner::MAX_TOKENS;
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
        let dyn_scan = scan_script(
            "const join=(a,b)=>a+b; fetch(join('/fresh-123/','dynamic')); function p(x){return '/v1/'+x;} location.href=p('users');",
            &base,
        );
        assert_eq!(
            dyn_scan.navigation,
            [
                "https://example.test/fresh-123/dynamic",
                "https://example.test/v1/users"
            ]
        );
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
    fn resolves_es6_template_literals_with_constants() {
        let base = Url::parse("https://example.test/app.js").unwrap();
        let scan = scan_script(
            "const prefix = '/api/v2'; fetch(`${prefix}/orders`);",
            &base,
        );
        assert_eq!(scan.navigation, ["https://example.test/api/v2/orders"]);
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
