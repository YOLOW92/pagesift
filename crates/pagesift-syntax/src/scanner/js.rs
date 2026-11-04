//! JavaScript 外部扫描器，移植自 tree-sitter-javascript 的 `src/scanner.c`（MIT，Copyright (c) 2014 Max Brunsfeld）。
//! 模板字符、JSX 文本、三元问号与 HTML 注释的扫描也被 TypeScript 的扫描器复用。

use super::{ScanLexer, Scanner, is_alpha, is_space};

pub(super) const AUTOMATIC_SEMICOLON: usize = 0;
pub(super) const TEMPLATE_CHARS: usize = 1;
pub(super) const TERNARY_QMARK: usize = 2;
pub(super) const HTML_COMMENT: usize = 3;
pub(super) const LOGICAL_OR: usize = 4;
pub(super) const ESCAPE_SEQUENCE: usize = 5;
pub(super) const REGEX_PATTERN: usize = 6;
pub(super) const JSX_TEXT: usize = 7;

pub struct Js;

fn is_nl(c: char) -> bool {
    matches!(c, '\n' | '\u{2028}' | '\u{2029}')
}

/// 模板字符串里的字面字符（两个语法共用）。
pub(super) fn template_chars(lx: &mut ScanLexer) -> bool {
    lx.set_result(TEMPLATE_CHARS as u16);
    let mut has_content = false;
    loop {
        lx.mark_end();
        match lx.lookahead() {
            '`' | '\\' => return has_content,
            '\0' => return false,
            '$' => {
                lx.next();
                if lx.lookahead() == '{' {
                    return has_content;
                }
            }
            _ => lx.next(),
        }
        has_content = true;
    }
}

#[derive(PartialEq)]
enum Ws {
    Reject,
    NoNewline,
    Accept,
}

fn ws_and_comments(lx: &mut ScanLexer, scanned_comment: &mut bool, consume: bool) -> Ws {
    let mut saw_block_newline = false;
    loop {
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if lx.lookahead() != '/' {
            return Ws::Accept;
        }
        lx.skip();
        if lx.lookahead() == '/' {
            lx.skip();
            while lx.lookahead() != '\0' && !is_nl(lx.lookahead()) {
                lx.skip();
            }
            *scanned_comment = true;
        } else if lx.lookahead() == '*' {
            lx.skip();
            while lx.lookahead() != '\0' {
                let c = lx.lookahead();
                if c == '*' {
                    lx.skip();
                    if lx.lookahead() == '/' {
                        lx.skip();
                        *scanned_comment = true;
                        if lx.lookahead() != '/' && !consume {
                            return if saw_block_newline {
                                Ws::Accept
                            } else {
                                Ws::NoNewline
                            };
                        }
                        break;
                    }
                } else if is_nl(c) {
                    saw_block_newline = true;
                    lx.skip();
                } else {
                    lx.skip();
                }
            }
        } else {
            return Ws::Reject;
        }
    }
}

fn automatic_semicolon(
    lx: &mut ScanLexer,
    comment_condition: bool,
    scanned_comment: &mut bool,
) -> bool {
    lx.set_result(AUTOMATIC_SEMICOLON as u16);
    lx.mark_end();
    loop {
        let c = lx.lookahead();
        if c == '\0' {
            return true;
        }
        if c == '/' {
            let r = ws_and_comments(lx, scanned_comment, false);
            if r == Ws::Reject {
                return false;
            }
            if r == Ws::Accept
                && comment_condition
                && lx.lookahead() != ','
                && lx.lookahead() != '='
            {
                return true;
            }
        }
        if lx.lookahead() == '}' {
            return true;
        }
        // is_at_included_range_start：整段输入只有一个范围，恒为假
        if is_nl(lx.lookahead()) {
            break;
        }
        if !is_space(lx.lookahead()) {
            return false;
        }
        lx.skip();
    }
    lx.skip();
    if ws_and_comments(lx, scanned_comment, true) == Ws::Reject {
        return false;
    }
    match lx.lookahead() {
        '`' | ',' | ':' | ';' | '*' | '%' | '>' | '<' | '=' | '[' | '(' | '?' | '^' | '|' | '&'
        | '/' => false,
        '.' => {
            lx.skip();
            lx.lookahead().is_ascii_digit()
        }
        '+' => {
            lx.skip();
            lx.lookahead() == '+'
        }
        '-' => {
            lx.skip();
            lx.lookahead() == '-'
        }
        '!' => {
            lx.skip();
            lx.lookahead() != '='
        }
        'i' => in_or_instanceof(lx),
        _ => true,
    }
}

/// 遇到 `i`：`in`、`instanceof` 之前不插分号，标识符之前插。
pub(super) fn in_or_instanceof(lx: &mut ScanLexer) -> bool {
    lx.skip();
    if lx.lookahead() != 'n' {
        return true;
    }
    lx.skip();
    if !is_alpha(lx.lookahead()) {
        return false;
    }
    for ch in "stanceof".chars() {
        if lx.lookahead() != ch {
            return true;
        }
        lx.skip();
    }
    is_alpha(lx.lookahead())
}

fn ternary_qmark(lx: &mut ScanLexer) -> bool {
    while is_space(lx.lookahead()) {
        lx.skip();
    }
    if lx.lookahead() == '?' {
        lx.next();
        if lx.lookahead() == '?' {
            return false;
        }
        lx.mark_end();
        lx.set_result(TERNARY_QMARK as u16);
        if lx.lookahead() == '.' {
            lx.next();
            return lx.lookahead().is_ascii_digit();
        }
        return true;
    }
    false
}

/// HTML 风格注释 `<!--` / `-->`（两个语法共用）。
pub(super) fn html_comment(lx: &mut ScanLexer) -> bool {
    while is_space(lx.lookahead()) || is_nl(lx.lookahead()) {
        lx.skip();
    }
    let pat = match lx.lookahead() {
        '<' => "<!--",
        '-' => "-->",
        _ => return false,
    };
    for ch in pat.chars() {
        if lx.lookahead() != ch {
            return false;
        }
        lx.next();
    }
    while lx.lookahead() != '\0' && !is_nl(lx.lookahead()) {
        lx.next();
    }
    lx.set_result(HTML_COMMENT as u16);
    lx.mark_end();
    true
}

pub(super) fn jsx_text(lx: &mut ScanLexer) -> bool {
    let (mut saw_text, mut at_newline) = (false, false);
    while !matches!(lx.lookahead(), '\0' | '<' | '>' | '{' | '}' | '&') {
        let ws = is_space(lx.lookahead());
        if lx.lookahead() == '\n' {
            at_newline = true;
        } else {
            at_newline &= ws;
            if !at_newline {
                saw_text = true;
            }
        }
        lx.next();
    }
    lx.set_result(JSX_TEXT as u16);
    saw_text
}

impl Scanner for Js {
    fn serialize(&self, _out: &mut Vec<u8>) {}
    fn deserialize(&mut self, _d: &[u8]) {}

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        if valid[TEMPLATE_CHARS] {
            if valid[AUTOMATIC_SEMICOLON] {
                return false;
            }
            return template_chars(lx);
        }
        if valid[JSX_TEXT] && jsx_text(lx) {
            return true;
        }
        if valid[AUTOMATIC_SEMICOLON] {
            let mut scanned_comment = false;
            let ret = automatic_semicolon(lx, !valid[LOGICAL_OR], &mut scanned_comment);
            if !ret && !scanned_comment && valid[TERNARY_QMARK] && lx.lookahead() == '?' {
                return ternary_qmark(lx);
            }
            return ret;
        }
        if valid[TERNARY_QMARK] {
            return ternary_qmark(lx);
        }
        if valid[HTML_COMMENT]
            && !valid[LOGICAL_OR]
            && !valid[ESCAPE_SEQUENCE]
            && !valid[REGEX_PATTERN]
        {
            return html_comment(lx);
        }
        false
    }
}
