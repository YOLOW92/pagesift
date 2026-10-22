//! 静态提取 fetch、axios、window.location、import 等网络调用端点与依赖。

use std::collections::HashMap;
use super::ast_scanner::Token;

#[derive(Debug, Clone)]
pub enum HelperPart {
    Literal(String),
    Param(usize),
    Const(String),
}

#[derive(Debug, Clone)]
pub struct Helper {
    pub params: usize,
    pub parts: Vec<HelperPart>,
}

/// 判断 Token 序列末尾是否匹配给定的模式词
pub fn ends(tokens: &[Token<'_>], pattern: &[&str]) -> bool {
    if tokens.len() < pattern.len() {
        return false;
    }
    let start = tokens.len() - pattern.len();
    if start > 0
        && matches!(tokens[start - 1], Token::Mark(b'.'))
        && !(start >= 2 && matches!(tokens[start - 2], Token::Word("window" | "axios" | "$")))
    {
        return false;
    }
    tokens[start..].iter().zip(pattern).all(|(t, p)| match t {
        Token::Word(w) => *w == *p,
        Token::Mark(c) => p.len() == 1 && p.as_bytes()[0] == *c,
        _ => false,
    })
}

/// 解析单行或简短辅助函数（如 `const join = (a, b) => a + b`）
pub fn parse_helper(
    tokens: &[Token<'_>],
    start: usize,
    arrow: bool,
    bindings: &HashMap<&str, String>,
) -> Option<Helper> {
    if !arrow && tokens.get(start) != Some(&Token::Mark(b'(')) {
        return None;
    }
    let (params, mut pos) = parse_params(tokens, start)?;
    if arrow {
        if tokens.get(pos..pos + 2) != Some(&[Token::Mark(b'='), Token::Mark(b'>')]) {
            return None;
        }
        pos += 2;
    }
    let block = tokens.get(pos) == Some(&Token::Mark(b'{'));
    if !arrow && !block {
        return None;
    }
    if block {
        if tokens.get(pos + 1) != Some(&Token::Word("return")) {
            return None;
        }
        pos += 2;
    }
    let mut parts = Vec::new();
    for _ in 0..16 {
        let part = match tokens.get(pos)? {
            Token::Literal(v) => HelperPart::Literal(v.clone()),
            Token::Word(w) => match params.iter().position(|p| p == w) {
                Some(idx) => HelperPart::Param(idx),
                None => HelperPart::Const(bindings.get(w)?.clone()),
            },
            _ => return None,
        };
        parts.push(part);
        pos += 1;
        if tokens.get(pos) != Some(&Token::Mark(b'+')) {
            if block {
                if tokens.get(pos) == Some(&Token::Mark(b';')) {
                    pos += 1;
                }
                if tokens.get(pos) != Some(&Token::Mark(b'}')) {
                    return None;
                }
            } else if !matches!(tokens.get(pos), Some(Token::Mark(b';')) | None) {
                return None;
            }
            return Some(Helper {
                params: params.len(),
                parts,
            });
        }
        pos += 1;
    }
    None
}

/// 解析形参列表
pub fn parse_params<'a>(tokens: &[Token<'a>], start: usize) -> Option<(Vec<&'a str>, usize)> {
    if let Some(Token::Word(single)) = tokens.get(start) {
        return Some((vec![*single], start + 1));
    }
    if tokens.get(start) != Some(&Token::Mark(b'(')) {
        return None;
    }
    let mut pos = start + 1;
    let mut params = Vec::new();
    if tokens.get(pos) == Some(&Token::Mark(b')')) {
        return Some((params, pos + 1));
    }
    while params.len() < 8 {
        let Some(Token::Word(name)) = tokens.get(pos) else {
            return None;
        };
        if params.contains(name) {
            return None;
        }
        params.push(*name);
        pos += 1;
        match tokens.get(pos)? {
            Token::Mark(b',') => pos += 1,
            Token::Mark(b')') => return Some((params, pos + 1)),
            _ => return None,
        }
    }
    None
}

/// 递归计算静态字符串拼接与辅助函数调用
pub fn expression(
    tokens: &[Token<'_>],
    start: usize,
    bindings: &HashMap<&str, String>,
    helpers: &HashMap<&str, Helper>,
    depth: usize,
) -> Option<(String, usize)> {
    if depth > 4 {
        return None;
    }
    let mut end = start;
    let mut value = String::new();
    for _ in 0..16 {
        let (part, next_end) = match tokens.get(end)? {
            Token::Literal(v) => (v.clone(), end + 1),
            Token::Template(tmpl) => {
                if let Some(evaled) = tmpl.try_eval(bindings) {
                    (evaled, end + 1)
                } else {
                    (tmpl.to_generalized_path(bindings), end + 1)
                }
            }
            Token::Word(name) if tokens.get(end + 1) == Some(&Token::Mark(b'(')) => {
                if end > 0 && tokens.get(end - 1) == Some(&Token::Mark(b'.')) {
                    return None;
                }
                let helper = helpers.get(name)?;
                let mut pos = end + 2;
                let mut args = Vec::with_capacity(helper.params);
                if helper.params == 0 {
                    if tokens.get(pos) != Some(&Token::Mark(b')')) {
                        return None;
                    }
                    pos += 1;
                } else {
                    for idx in 0..helper.params {
                        let (arg, after_arg) =
                            expression(tokens, pos, bindings, helpers, depth + 1)?;
                        args.push(arg);
                        let expected = if idx + 1 == helper.params { b')' } else { b',' };
                        if tokens.get(after_arg) != Some(&Token::Mark(expected)) {
                            return None;
                        }
                        pos = after_arg + 1;
                    }
                }
                let mut joined = String::new();
                for hp in &helper.parts {
                    let s = match hp {
                        HelperPart::Literal(lit) => lit.as_str(),
                        HelperPart::Param(idx) => args.get(*idx)?.as_str(),
                        HelperPart::Const(c) => c.as_str(),
                    };
                    if joined.len() + s.len() > 8192 {
                        return None;
                    }
                    joined.push_str(s);
                }
                (joined, pos)
            }
            Token::Word(name) => (bindings.get(name)?.clone(), end + 1),
            _ => return None,
        };
        if value.len() + part.len() > 8192 {
            return None;
        }
        value.push_str(&part);
        end = next_end;
        if tokens.get(end) != Some(&Token::Mark(b'+')) {
            return Some((value, end));
        }
        end += 1;
    }
    None
}
