//! TypeScript / TSX 外部扫描器，移植自 tree-sitter-typescript 的 `common/scanner.h`
//! （MIT，Copyright (c) 2017 Max Brunsfeld）。模板字符、JSX 文本与 HTML 注释和 JavaScript 共用。

use super::js::{
    AUTOMATIC_SEMICOLON, ESCAPE_SEQUENCE, HTML_COMMENT, JSX_TEXT, LOGICAL_OR, REGEX_PATTERN,
    TEMPLATE_CHARS, TERNARY_QMARK, html_comment, in_or_instanceof, jsx_text, template_chars,
};
use super::{ScanLexer, Scanner, is_space};

const FUNCTION_SIGNATURE_AUTOMATIC_SEMICOLON: usize = 8;

pub struct Ts;

fn ws_and_comments(lx: &mut ScanLexer, scanned_comment: &mut bool) -> bool {
    loop {
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if lx.lookahead() != '/' {
            return true;
        }
        lx.skip();
        if lx.lookahead() == '/' {
            lx.skip();
            while lx.lookahead() != '\0' && lx.lookahead() != '\n' {
                lx.skip();
            }
            *scanned_comment = true;
        } else if lx.lookahead() == '*' {
            lx.skip();
            while lx.lookahead() != '\0' {
                if lx.lookahead() == '*' {
                    lx.skip();
                    if lx.lookahead() == '/' {
                        lx.skip();
                        break;
                    }
                } else {
                    lx.skip();
                }
            }
        } else {
            return false;
        }
    }
}

fn automatic_semicolon(lx: &mut ScanLexer, valid: &[bool], scanned_comment: &mut bool) -> bool {
    lx.set_result(AUTOMATIC_SEMICOLON as u16);
    lx.mark_end();
    loop {
        if lx.lookahead() == '\0' {
            return true;
        }
        if lx.lookahead() == '}' {
            // 带类型的对象模式里不能自动补分号：`type F = ({a}: {a: number}) => number;`
            loop {
                lx.skip();
                if !is_space(lx.lookahead()) {
                    break;
                }
            }
            if lx.lookahead() == ':' {
                return valid[LOGICAL_OR];
            }
            return true;
        }
        if !is_space(lx.lookahead()) {
            return false;
        }
        if lx.lookahead() == '\n' {
            break;
        }
        lx.skip();
    }
    lx.skip();
    if !ws_and_comments(lx, scanned_comment) {
        return false;
    }
    match lx.lookahead() {
        '`' | ',' | '.' | ';' | '*' | '%' | '>' | '<' | '=' | '?' | '^' | '|' | '&' | '/' | ':' => {
            return false;
        }
        '{' => {
            if valid[FUNCTION_SIGNATURE_AUTOMATIC_SEMICOLON] {
                return false;
            }
        }
        '(' | '[' => {
            if valid[LOGICAL_OR] {
                return false;
            }
        }
        '+' => {
            lx.skip();
            return lx.lookahead() == '+';
        }
        '-' => {
            lx.skip();
            return lx.lookahead() == '-';
        }
        '!' => {
            lx.skip();
            return lx.lookahead() != '=';
        }
        'i' => return in_or_instanceof(lx),
        _ => {}
    }
    true
}

fn ternary_qmark(lx: &mut ScanLexer) -> bool {
    while is_space(lx.lookahead()) {
        lx.skip();
    }
    if lx.lookahead() != '?' {
        return false;
    }
    lx.next();
    if lx.lookahead() == '?' || lx.lookahead() == '.' {
        return false;
    }
    lx.mark_end();
    lx.set_result(TERNARY_QMARK as u16);
    // TypeScript 的可选参数含 `?:`，中间可有空白
    while is_space(lx.lookahead()) {
        lx.next();
    }
    if matches!(lx.lookahead(), ':' | ')' | ',') {
        return false;
    }
    if lx.lookahead() == '.' {
        lx.next();
        return lx.lookahead().is_ascii_digit();
    }
    true
}

impl Scanner for Ts {
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
        if valid[AUTOMATIC_SEMICOLON] || valid[FUNCTION_SIGNATURE_AUTOMATIC_SEMICOLON] {
            let mut scanned_comment = false;
            let ret = automatic_semicolon(lx, valid, &mut scanned_comment);
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
