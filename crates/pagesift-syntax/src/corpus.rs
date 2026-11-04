//! M3：解析 tree-sitter 语法仓库的 `test/corpus/*.txt` 并和期望的 S 表达式比对。
//! 判定规则与官方 `tree-sitter test` 保持一致（空白归一、期望里没有字段名就去掉实际输出的字段名）。

use crate::Limits;
use crate::api::parse;
use crate::table::Language;

pub struct Case {
    pub name: String,
    pub input: Vec<u8>,
    pub expected: String,
    pub skip: bool,
    pub error: bool,
    pub cst: bool,
    pub platform_ok: bool,
    /// `:language(xxx)`：只对指定名字的语法运行。
    pub language: Option<String>,
}

fn delim(line: &str, c: char) -> Option<(usize, &str)> {
    let n = line.len() - line.trim_start_matches(c).len();
    if n < 3 {
        return None;
    }
    Some((n, line[n..].trim_end_matches(['\r', '\n'])))
}

fn suffix_matches(first: Option<&str>, suffix: &str) -> bool {
    match (first, suffix.is_empty()) {
        (None, true) => true,
        (Some(f), false) => f == suffix,
        _ => false,
    }
}

struct Pending {
    name: String,
    skip: bool,
    error: bool,
    cst: bool,
    platform_ok: bool,
    language: Option<String>,
    body_start: usize,
}

fn parse_header(lines: &[&str], first: Option<&str>, start: usize) -> Option<Pending> {
    let (_, suffix) = delim(lines[start], '=')?;
    if !suffix_matches(first, suffix) {
        return None;
    }
    let (mut name, mut marker, mut skip, mut error, mut cst) =
        (String::new(), false, false, false, false);
    let mut platform: Option<bool> = None;
    let mut language: Option<String> = None;
    let mut i = start + 1;
    while i < lines.len() {
        if let Some((_, cs)) = delim(lines[i], '=') {
            if suffix_matches(first, cs) {
                break;
            }
        }
        let t = lines[i].trim();
        if t.is_empty() && !marker {
            return None;
        }
        match t.split('(').next().unwrap_or("") {
            ":skip" => (marker, skip) = (true, true),
            ":platform" => {
                if let Some(p) = t
                    .strip_prefix(":platform(")
                    .and_then(|s| s.strip_suffix(')'))
                {
                    marker = true;
                    platform = Some(platform.unwrap_or(false) || p.trim() == std::env::consts::OS);
                }
            }
            ":fail-fast" => marker = true,
            ":error" => (marker, error) = (true, true),
            ":language" => {
                marker = true;
                language = t
                    .strip_prefix(":language(")
                    .and_then(|s| s.strip_suffix(')'))
                    .map(|s| s.trim().to_string());
            }
            ":cst" => (marker, cst) = (true, true),
            _ if !marker => name.push_str(lines[i]),
            _ => {}
        }
        i += 1;
    }
    if i >= lines.len() {
        return None;
    }
    if skip {
        error = false;
    }
    Some(Pending {
        name: name.trim_end().to_string(),
        skip,
        error,
        cst,
        platform_ok: platform.unwrap_or(true),
        language,
        body_start: i + 1,
    })
}

fn build(body: &[&str], first: Option<&str>, p: Pending) -> Option<Case> {
    let mut best: Option<usize> = None;
    let mut best_len = 0;
    for (j, line) in body.iter().enumerate() {
        if let Some((n, s)) = delim(line, '-') {
            if suffix_matches(first, s) && n + s.len() >= best_len {
                best = Some(j);
                best_len = n + s.len();
            }
        }
    }
    let d = best?;
    let mut input: Vec<u8> = body[..d]
        .iter()
        .flat_map(|l| l.as_bytes())
        .copied()
        .collect();
    if input.last() == Some(&b'\n') {
        input.pop();
    }
    if input.last() == Some(&b'\r') {
        input.pop();
    }
    let out: String = body[d + 1..].iter().copied().collect();
    let expected = if p.cst {
        out.trim().to_string()
    } else {
        normalize(&out)
    };
    Some(Case {
        name: p.name,
        input,
        expected,
        skip: p.skip,
        error: p.error,
        cst: p.cst,
        platform_ok: p.platform_ok,
        language: p.language,
    })
}

fn normalize(raw: &str) -> String {
    let mut r = String::new();
    let mut prev_space = false;
    for line in raw.lines() {
        if line.trim_start().starts_with(';') {
            continue;
        }
        for ch in line.chars() {
            if ch.is_whitespace() {
                if !prev_space && !r.is_empty() {
                    r.push(' ');
                    prev_space = true;
                }
            } else {
                if ch == ')' && prev_space {
                    r.pop();
                }
                r.push(ch);
                prev_space = false;
            }
        }
        if !r.is_empty() && !prev_space {
            r.push(' ');
            prev_space = true;
        }
    }
    r.trim_end().to_string()
}

pub fn parse_corpus(content: &str) -> Vec<Case> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let first = lines.iter().find_map(|l| match delim(l, '=')? {
        (_, s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    });
    let first = first.as_deref();
    let mut out = Vec::new();
    let mut prev: Option<Pending> = None;
    let mut i = 0;
    while i < lines.len() {
        let Some(p) = parse_header(&lines, first, i) else {
            i += 1;
            continue;
        };
        let opening = i;
        i = p.body_start;
        if let Some(pr) = prev.take() {
            if let Some(c) = build(&lines[pr.body_start..opening], first, pr) {
                out.push(c);
            }
        }
        prev = Some(p);
    }
    if let Some(pr) = prev {
        if let Some(c) = build(&lines[pr.body_start..], first, pr) {
            out.push(c);
        }
    }
    out
}

/// 去掉 ` word: (` 里的字段名，规则同官方 `strip_sexp_fields`。
pub fn strip_fields(s: &str) -> String {
    let mut r = String::with_capacity(s.len());
    let mut rem = s;
    while let Some(pos) = rem.find(": (") {
        if let Some(sp) = rem[..pos].rfind(' ') {
            let w = &rem[sp + 1..pos];
            if !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                r.push_str(&rem[..=sp]);
                r.push('(');
                rem = &rem[pos + 3..];
                continue;
            }
        }
        r.push_str(&rem[..pos + 3]);
        rem = &rem[pos + 3..];
    }
    r.push_str(rem);
    r
}

#[derive(Default)]
pub struct Report {
    pub total: usize,
    pub passed: usize,
    pub skipped: usize,
    pub failures: Vec<(String, String, String)>,
}

pub fn run_cases(lang: &Language, file: &str, cases: &[Case], rep: &mut Report) {
    for c in cases {
        if c.skip
            || c.cst
            || !c.platform_ok
            || c.language.as_deref().is_some_and(|l| l != lang.name)
        {
            rep.skipped += 1;
            continue;
        }
        rep.total += 1;
        let tree = parse(lang, &c.input, Limits::default());
        let ok;
        let mut actual = String::new();
        if c.error {
            ok = tree.has_error();
        } else {
            let has_fields = c.expected.contains(": (");
            let s = tree.to_sexp(lang);
            actual = if has_fields { s } else { strip_fields(&s) };
            ok = actual == c.expected;
        }
        if ok {
            rep.passed += 1;
        } else {
            rep.failures
                .push((format!("{file}: {}", c.name), c.expected.clone(), actual));
        }
    }
}
