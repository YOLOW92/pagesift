//! R 外部扫描器，移植自 tree-sitter-r 的 `src/scanner.c`（MIT，Copyright (c) 2025 tree-sitter-r authors）。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_space};

const START: usize = 0;
const NEWLINE: usize = 1;
const SEMICOLON: usize = 2;
const RAW_STRING_OPEN: usize = 3;
const RAW_STRING_CONTENT: usize = 4;
const RAW_STRING_CLOSE: usize = 5;
const ELSE: usize = 6;
const OPEN_PAREN: usize = 7;
const CLOSE_PAREN: usize = 8;
const OPEN_BRACE: usize = 9;
const CLOSE_BRACE: usize = 10;
const OPEN_BRACKET: usize = 11;
const CLOSE_BRACKET: usize = 12;
const OPEN_BRACKET2: usize = 13;
const CLOSE_BRACKET2: usize = 14;
const ERROR_SENTINEL: usize = 15;

const SCOPE_BRACE: u8 = 1;
const SCOPE_PAREN: u8 = 2;
const SCOPE_BRACKET: u8 = 3;
const SCOPE_BRACKET2: u8 = 4;
/// 序列化缓冲里除作用域栈之外还要放：括号、连字符个数、引号各一字节，加四字节的栈深度。
const MAX_SCOPES: usize = MAX_SERIALIZED - 3 - 4;

#[derive(Default)]
pub struct R {
    closing_bracket: u8,
    hyphen_count: u8,
    closing_quote: u8,
    /// 栈底的顶层作用域不入栈。
    scopes: Vec<u8>,
}

fn is_identifier_continuation(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.' || c as u32 >= 128
}

fn scan_else(lx: &mut ScanLexer) -> bool {
    for ch in "else".chars() {
        if lx.lookahead() != ch {
            return false;
        }
        lx.next();
    }
    // `else_idx` 这样的更长标识符不算
    if is_identifier_continuation(lx.lookahead()) {
        return false;
    }
    lx.mark_end();
    lx.set_result(ELSE as u16);
    true
}

fn scan_else_with_leading_newlines(lx: &mut ScanLexer) -> bool {
    while is_space(lx.lookahead()) {
        if lx.lookahead() != '\n' {
            lx.skip();
            continue;
        }
        lx.skip();
        lx.mark_end();
        lx.set_result(NEWLINE as u16);
    }
    // 后面是注释就交给内部扫描器
    if lx.lookahead() == '#' {
        return false;
    }
    scan_else(lx);
    true
}

impl R {
    fn peek(&self) -> u8 {
        self.scopes.last().copied().unwrap_or(0)
    }

    fn push(&mut self, scope: u8) -> bool {
        if self.scopes.len() >= MAX_SCOPES {
            return false;
        }
        self.scopes.push(scope);
        true
    }

    fn pop(&mut self, expected: u8) -> bool {
        match self.scopes.pop() {
            Some(s) => s == expected,
            None => false,
        }
    }

    fn consume_whitespace(&self, lx: &mut ScanLexer) {
        while is_space(lx.lookahead()) {
            if lx.lookahead() != '\n' {
                lx.skip();
                continue;
            }
            // `(`、`[`、`[[` 里的换行没有语法意义，直接跳过；顶层和 `{` 里的换行留给后面的分支
            if matches!(self.peek(), SCOPE_PAREN | SCOPE_BRACKET | SCOPE_BRACKET2) {
                lx.skip();
                continue;
            }
            break;
        }
    }

    fn scan_raw_string_open(&mut self, lx: &mut ScanLexer) -> bool {
        if !matches!(lx.lookahead(), 'r' | 'R') {
            return false;
        }
        lx.next();
        let quote = lx.lookahead();
        if quote != '"' && quote != '\'' {
            return false;
        }
        lx.next();
        let mut hyphens = 0u8;
        while lx.lookahead() == '-' {
            if hyphens == u8::MAX {
                return false;
            }
            lx.next();
            hyphens += 1;
        }
        let close = match lx.lookahead() {
            '(' => b')',
            '[' => b']',
            '{' => b'}',
            _ => return false,
        };
        lx.next();
        lx.mark_end();
        lx.set_result(RAW_STRING_OPEN as u16);
        self.closing_bracket = close;
        self.hyphen_count = hyphens;
        self.closing_quote = quote as u8;
        true
    }

    fn scan_raw_string_content_or_close(&mut self, lx: &mut ScanLexer) -> bool {
        let mut any_content = false;
        while !lx.eof() {
            if lx.lookahead() != self.closing_bracket as char {
                lx.next();
                any_content = true;
                continue;
            }
            lx.mark_end();
            lx.next();
            let mut matched = true;
            for _ in 0..self.hyphen_count {
                if lx.lookahead() != '-' {
                    matched = false;
                    break;
                }
                lx.next();
            }
            if !matched || lx.lookahead() != self.closing_quote as char {
                any_content = true;
                continue;
            }
            lx.next();
            if any_content {
                lx.set_result(RAW_STRING_CONTENT as u16);
            } else {
                lx.mark_end();
                lx.set_result(RAW_STRING_CLOSE as u16);
            }
            return true;
        }
        false
    }

    fn scan_raw_string_close(&mut self, lx: &mut ScanLexer) -> bool {
        lx.next();
        for _ in 0..self.hyphen_count {
            lx.next();
        }
        lx.next();
        lx.mark_end();
        lx.set_result(RAW_STRING_CLOSE as u16);
        true
    }

    fn open_block(&mut self, lx: &mut ScanLexer, scope: u8, sym: usize) -> bool {
        if !self.push(scope) {
            return false;
        }
        lx.next();
        lx.mark_end();
        lx.set_result(sym as u16);
        true
    }

    fn close_block(&mut self, lx: &mut ScanLexer, scope: u8, sym: usize) -> bool {
        if !self.pop(scope) {
            return false;
        }
        lx.next();
        lx.mark_end();
        lx.set_result(sym as u16);
        true
    }

    fn open_bracket(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        lx.next();
        if v[OPEN_BRACKET2] && lx.lookahead() == '[' {
            if !self.push(SCOPE_BRACKET2) {
                return false;
            }
            lx.next();
            lx.mark_end();
            lx.set_result(OPEN_BRACKET2 as u16);
            return true;
        }
        if v[OPEN_BRACKET] {
            if !self.push(SCOPE_BRACKET) {
                return false;
            }
            lx.mark_end();
            lx.set_result(OPEN_BRACKET as u16);
            return true;
        }
        false
    }

    fn close_bracket2(&mut self, lx: &mut ScanLexer) -> bool {
        lx.next();
        if lx.lookahead() != ']' {
            return false;
        }
        self.close_block(lx, SCOPE_BRACKET2, CLOSE_BRACKET2)
    }
}

impl Scanner for R {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        // 错误恢复时所有符号都有效，不处理
        if v[ERROR_SENTINEL] {
            return false;
        }
        if v[START] {
            lx.set_result(START as u16);
            return true;
        }
        if v[RAW_STRING_CONTENT] {
            return self.scan_raw_string_content_or_close(lx);
        } else if v[RAW_STRING_CLOSE] {
            return self.scan_raw_string_close(lx);
        }
        self.consume_whitespace(lx);
        let c = lx.lookahead();
        if v[SEMICOLON] && c == ';' {
            lx.next();
            lx.mark_end();
            lx.set_result(SEMICOLON as u16);
            true
        } else if v[OPEN_PAREN] && c == '(' {
            self.open_block(lx, SCOPE_PAREN, OPEN_PAREN)
        } else if v[CLOSE_PAREN] && c == ')' {
            self.close_block(lx, SCOPE_PAREN, CLOSE_PAREN)
        } else if v[OPEN_BRACE] && c == '{' {
            self.open_block(lx, SCOPE_BRACE, OPEN_BRACE)
        } else if v[CLOSE_BRACE] && c == '}' {
            self.close_block(lx, SCOPE_BRACE, CLOSE_BRACE)
        } else if (v[OPEN_BRACKET] || v[OPEN_BRACKET2]) && c == '[' {
            self.open_bracket(lx, v)
        } else if v[CLOSE_BRACKET] && c == ']' && self.peek() == SCOPE_BRACKET {
            self.close_block(lx, SCOPE_BRACKET, CLOSE_BRACKET)
        } else if v[CLOSE_BRACKET2] && c == ']' && self.peek() == SCOPE_BRACKET2 {
            self.close_bracket2(lx)
        } else if v[RAW_STRING_OPEN] && matches!(c, 'r' | 'R') {
            self.scan_raw_string_open(lx)
        } else if v[ELSE] && c == 'e' {
            scan_else(lx)
        } else if v[ELSE] && self.peek() == SCOPE_BRACE && c == '\n' {
            scan_else_with_leading_newlines(lx)
        } else if v[NEWLINE] && c == '\n' {
            lx.next();
            lx.mark_end();
            lx.set_result(NEWLINE as u16);
            true
        } else {
            false
        }
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.closing_bracket, self.hyphen_count, self.closing_quote]);
        out.extend_from_slice(&(self.scopes.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.scopes);
    }

    fn deserialize(&mut self, d: &[u8]) {
        let reset = |s: &mut R| {
            s.closing_bracket = 0;
            s.hyphen_count = 0;
            s.closing_quote = 0;
            s.scopes.clear();
        };
        if d.is_empty() {
            reset(self);
            return;
        }
        if d.len() < 7 {
            reset(self);
            return;
        }
        self.closing_bracket = d[0];
        self.hyphen_count = d[1];
        self.closing_quote = d[2];
        let n = u32::from_le_bytes([d[3], d[4], d[5], d[6]]) as usize;
        if n > d.len() - 7 {
            reset(self);
            return;
        }
        self.scopes = d[7..7 + n].to_vec();
    }
}
