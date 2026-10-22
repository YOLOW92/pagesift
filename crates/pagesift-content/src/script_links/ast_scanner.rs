//! 自研纯 Rust 流式 ECMAScript Lexer 状态机（无重型 V8/SWC 依赖）。

use super::template_literals::{TemplateLiteral, TemplatePart};

pub const MAX_SCRIPT: usize = 1024 * 1024;
pub const MAX_TOKENS: usize = 65_536;

#[derive(Debug, PartialEq, Clone)]
pub enum Token<'a> {
    Word(&'a str),
    Literal(String),
    Template(TemplateLiteral),
    Mark(u8),
}

/// 快速流式词法分析器。
pub fn lex(source: &str) -> Vec<Token<'_>> {
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

        // 单行注释
        if b[i..].starts_with(b"//") {
            i += 2;
            while i < b.len() && !matches!(b[i], b'\n' | b'\r') {
                i += 1;
            }
            continue;
        }

        // 多行注释
        if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }

        // 字符串字面量与 ES6 反引号模板字符串
        if matches!(b[i], b'\'' | b'"' | b'`') {
            let quote = b[i];
            i += 1;

            if quote == b'`' {
                // ES6 模板字符串解析
                let mut template = TemplateLiteral::default();
                let mut text = Vec::new();
                let mut valid = true;
                let mut closed = false;

                while i < b.len() {
                    let c = b[i];
                    i += 1;
                    if c == b'`' {
                        closed = true;
                        if !text.is_empty() {
                            template.parts.push(TemplatePart::Literal(
                                String::from_utf8(text.clone()).unwrap_or_default(),
                            ));
                            text.clear();
                        }
                        break;
                    }
                    if c == b'$' && b.get(i) == Some(&b'{') {
                        if !text.is_empty() {
                            template.parts.push(TemplatePart::Literal(
                                String::from_utf8(text.clone()).unwrap_or_default(),
                            ));
                            text.clear();
                        }
                        i += 1; // 跳过 '{'
                        let start_expr = i;
                        let mut brace_depth = 1usize;
                        while i < b.len() && brace_depth > 0 {
                            if b[i] == b'`' || b[i] == b'\n' {
                                valid = false;
                                break;
                            }
                            if b[i] == b'{' {
                                brace_depth += 1;
                            } else if b[i] == b'}' {
                                brace_depth -= 1;
                            }
                            i += 1;
                        }
                        if brace_depth == 0 {
                            let expr_bytes = &b[start_expr..i - 1];
                            let expr_str = std::str::from_utf8(expr_bytes).unwrap_or("").trim();
                            template.parts.push(TemplatePart::Expr(expr_str.to_string()));
                        } else {
                            valid = false;
                            break;
                        }
                        continue;
                    }
                    if c == b'\\' {
                        let Some(&next) = b.get(i) else { break };
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

                if valid && closed {
                    if template.is_static() {
                        let lit = template
                            .parts
                            .into_iter()
                            .filter_map(|p| match p {
                                TemplatePart::Literal(s) => Some(s),
                                _ => None,
                            })
                            .collect::<String>();
                        out.push(Token::Literal(lit));
                    } else {
                        out.push(Token::Template(template));
                    }
                } else {
                    out.push(Token::Mark(b'?'));
                }
                continue;
            }

            // 单引号与双引号字符串字面量
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
                if c == b'\\' {
                    let Some(&next) = b.get(i) else { break };
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

        // 正则字面量：在表达式开始位置消费完整的正则，不将其内部内容误判为代码
        if b[i] == b'/'
            && out.last().is_none_or(|t| {
                matches!(
                    t,
                    Token::Mark(b'=' | b'(' | b'[' | b':' | b',' | b'!' | b';')
                ) || matches!(t, Token::Word("return"))
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

        // 标识符与保留字
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
