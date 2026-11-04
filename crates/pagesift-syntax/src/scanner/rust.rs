//! Rust 外部扫描器，移植自 tree-sitter-rust 的 `src/scanner.c`（MIT，Copyright (c) 2017 Maxim Sokolov）。

use super::{ScanLexer, Scanner, is_alpha, is_space};

const STRING_CONTENT: usize = 0;
const STRING_CLOSE: usize = 1;
const RAW_STRING_LITERAL_START: usize = 2;
const RAW_STRING_LITERAL_CONTENT: usize = 3;
const RAW_STRING_LITERAL_END: usize = 4;
const FLOAT_LITERAL: usize = 5;
const BLOCK_OUTER_DOC_MARKER: usize = 6;
const BLOCK_INNER_DOC_MARKER: usize = 7;
const BLOCK_COMMENT_CONTENT: usize = 8;
const LINE_DOC_CONTENT: usize = 9;
const ERROR_SENTINEL: usize = 10;

#[derive(Default)]
pub struct Rust {
    opening_hash_count: u8,
}

fn is_num_char(c: char) -> bool {
    c == '_' || c.is_ascii_digit()
}

fn process_string(lx: &mut ScanLexer) -> bool {
    let mut has_content = false;
    loop {
        if lx.lookahead() == '"' || lx.lookahead() == '\\' {
            break;
        }
        if lx.eof() {
            return false;
        }
        has_content = true;
        lx.next();
    }
    lx.set_result(STRING_CONTENT as u16);
    lx.mark_end();
    has_content
}

fn process_float(lx: &mut ScanLexer) -> bool {
    lx.set_result(FLOAT_LITERAL as u16);
    lx.next();
    while is_num_char(lx.lookahead()) {
        lx.next();
    }
    let (mut has_fraction, mut has_exponent) = (false, false);
    if lx.lookahead() == '.' {
        has_fraction = true;
        lx.next();
        if is_alpha(lx.lookahead()) || lx.lookahead() == '.' {
            return false;
        }
        while is_num_char(lx.lookahead()) {
            lx.next();
        }
    }
    lx.mark_end();
    if lx.lookahead() == 'e' || lx.lookahead() == 'E' {
        has_exponent = true;
        lx.next();
        if lx.lookahead() == '+' || lx.lookahead() == '-' {
            lx.next();
        }
        if !is_num_char(lx.lookahead()) {
            return true;
        }
        lx.next();
        while is_num_char(lx.lookahead()) {
            lx.next();
        }
        lx.mark_end();
    }
    if !has_exponent && !has_fraction {
        return false;
    }
    if !matches!(lx.lookahead(), 'u' | 'i' | 'f') {
        return true;
    }
    lx.next();
    if !lx.lookahead().is_ascii_digit() {
        return true;
    }
    while lx.lookahead().is_ascii_digit() {
        lx.next();
    }
    lx.mark_end();
    true
}

fn line_doc_content(lx: &mut ScanLexer) -> bool {
    lx.set_result(LINE_DOC_CONTENT as u16);
    loop {
        if lx.eof() {
            return true;
        }
        if lx.lookahead() == '\n' {
            lx.next();
            return true;
        }
        lx.next();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Bc {
    LeftSlash,
    LeftAsterisk,
    Continuing,
}

/// C 里是 `(char)lexer->lookahead`，只取低 8 位；保持同样的截断行为。
fn low8(c: char) -> u8 {
    (c as u32 & 0xFF) as u8
}

fn block_comment(lx: &mut ScanLexer, valid: &[bool]) -> bool {
    let mut first = low8(lx.lookahead());
    if valid[BLOCK_INNER_DOC_MARKER] && first == b'!' {
        lx.set_result(BLOCK_INNER_DOC_MARKER as u16);
        lx.next();
        return true;
    }
    if valid[BLOCK_OUTER_DOC_MARKER] && first == b'*' {
        lx.next();
        lx.mark_end();
        if lx.lookahead() == '/' {
            return false;
        }
        if lx.lookahead() != '*' {
            lx.set_result(BLOCK_OUTER_DOC_MARKER as u16);
            return true;
        }
    } else {
        lx.next();
    }
    if valid[BLOCK_COMMENT_CONTENT] {
        let mut depth: u32 = 1;
        let mut state = match first {
            b'*' => {
                if lx.lookahead() == '/' {
                    return false;
                }
                Bc::LeftAsterisk
            }
            b'/' => Bc::LeftSlash,
            _ => Bc::Continuing,
        };
        while !lx.eof() && depth != 0 {
            first = low8(lx.lookahead());
            match state {
                Bc::LeftSlash => {
                    if first == b'*' {
                        depth += 1;
                    }
                    state = Bc::Continuing;
                }
                Bc::LeftAsterisk => {
                    if first == b'*' {
                        lx.mark_end();
                        state = Bc::LeftAsterisk;
                    } else {
                        if first == b'/' {
                            depth = depth.wrapping_sub(1);
                        }
                        state = Bc::Continuing;
                    }
                }
                Bc::Continuing => {
                    lx.mark_end();
                    match first {
                        b'/' => state = Bc::LeftSlash,
                        b'*' => state = Bc::LeftAsterisk,
                        _ => {}
                    }
                }
            }
            lx.next();
            if first == b'/' && depth != 0 {
                lx.mark_end();
            }
        }
        lx.set_result(BLOCK_COMMENT_CONTENT as u16);
        return true;
    }
    false
}

impl Scanner for Rust {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.opening_hash_count);
    }
    fn deserialize(&mut self, d: &[u8]) {
        self.opening_hash_count = if d.len() == 1 { d[0] } else { 0 };
    }

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        if valid[ERROR_SENTINEL] {
            return false;
        }
        if valid[BLOCK_COMMENT_CONTENT]
            || valid[BLOCK_INNER_DOC_MARKER]
            || valid[BLOCK_OUTER_DOC_MARKER]
        {
            return block_comment(lx, valid);
        }
        if valid[STRING_CONTENT] && !valid[FLOAT_LITERAL] && process_string(lx) {
            return true;
        }
        if valid[STRING_CLOSE] && lx.lookahead() == '"' {
            lx.next();
            lx.set_result(STRING_CLOSE as u16);
            lx.mark_end();
            return true;
        }
        if valid[LINE_DOC_CONTENT] {
            return line_doc_content(lx);
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if valid[RAW_STRING_LITERAL_START] && matches!(lx.lookahead(), 'r' | 'b' | 'c') {
            // 原始字符串开头
            if lx.lookahead() == 'b' || lx.lookahead() == 'c' {
                lx.next();
            }
            if lx.lookahead() != 'r' {
                return false;
            }
            lx.next();
            let mut hashes: u8 = 0;
            while lx.lookahead() == '#' {
                lx.next();
                hashes = hashes.wrapping_add(1);
            }
            if lx.lookahead() != '"' {
                return false;
            }
            lx.next();
            self.opening_hash_count = hashes;
            lx.set_result(RAW_STRING_LITERAL_START as u16);
            return true;
        }
        if valid[RAW_STRING_LITERAL_CONTENT] {
            loop {
                if lx.eof() {
                    return false;
                }
                if lx.lookahead() == '"' {
                    lx.mark_end();
                    lx.next();
                    let mut n = 0u32;
                    while lx.lookahead() == '#' && n < self.opening_hash_count as u32 {
                        lx.next();
                        n += 1;
                    }
                    if n == self.opening_hash_count as u32 {
                        lx.set_result(RAW_STRING_LITERAL_CONTENT as u16);
                        return true;
                    }
                } else {
                    lx.next();
                }
            }
        }
        if valid[RAW_STRING_LITERAL_END] && lx.lookahead() == '"' {
            lx.next();
            for _ in 0..self.opening_hash_count {
                lx.next();
            }
            lx.set_result(RAW_STRING_LITERAL_END as u16);
            return true;
        }
        if valid[FLOAT_LITERAL] && lx.lookahead().is_ascii_digit() {
            return process_float(lx);
        }
        false
    }
}
