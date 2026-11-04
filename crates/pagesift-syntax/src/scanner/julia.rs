//! Julia 外部扫描器，移植自 tree-sitter-julia 的 `src/scanner.c`（MIT，Copyright (c) 2018 Max Brunsfeld, GitHub）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner};

const BLOCK_COMMENT_REST: usize = 0;
const IMMEDIATE_PAREN: usize = 1;
const IMMEDIATE_BRACKET: usize = 2;
const IMMEDIATE_BRACE: usize = 3;
const IMMEDIATE_STRING_START: usize = 4;
const IMMEDIATE_COMMAND_START: usize = 5;
const CONTENT_CMD_1: usize = 6;
const CONTENT_CMD_1_RAW: usize = 7;
const CONTENT_CMD_3: usize = 8;
const CONTENT_CMD_3_RAW: usize = 9;
const CONTENT_STR_1: usize = 10;
const CONTENT_STR_1_RAW: usize = 11;
const CONTENT_STR_3: usize = 12;
const CONTENT_STR_3_RAW: usize = 13;
const END_CMD: usize = 14;
const END_STR: usize = 15;

#[derive(Default)]
pub struct Julia;

fn scan_content(
    lx: &mut ScanLexer,
    content: usize,
    end_char: char,
    n_delim: u32,
    interp: bool,
) -> bool {
    let end_symbol = if end_char == '"' { END_STR } else { END_CMD };
    let mut has_content = false;
    while lx.lookahead() != '\0' {
        let next = lx.lookahead();
        lx.mark_end();
        if interp && (next == '$' || next == '\\') {
            lx.set_result(content as u16);
            return has_content;
        } else if next == '\\' {
            // 原始字符串里的反斜杠：检查被转义的分隔符和 `\\`
            lx.next();
            let c = lx.lookahead();
            if c == end_char || c == '\\' {
                lx.set_result(content as u16);
                return has_content;
            }
        } else {
            let mut is_end = true;
            for _ in 0..n_delim {
                if lx.lookahead() == end_char {
                    lx.next();
                } else {
                    is_end = false;
                    break;
                }
            }
            if is_end {
                if has_content {
                    lx.set_result(content as u16);
                } else {
                    lx.mark_end();
                    lx.set_result(end_symbol as u16);
                }
                return true;
            }
        }
        lx.next();
        has_content = true;
    }
    false
}

fn scan_block_comment(lx: &mut ScanLexer) -> bool {
    let mut after_eq = false;
    let mut depth = 1u32;
    loop {
        match lx.lookahead() {
            '=' => {
                lx.next();
                after_eq = true;
            }
            '#' => {
                lx.next();
                if after_eq {
                    after_eq = false;
                    depth -= 1;
                    if depth == 0 {
                        lx.set_result(BLOCK_COMMENT_REST as u16);
                        return true;
                    }
                } else {
                    after_eq = false;
                    if lx.lookahead() == '=' {
                        lx.next();
                        depth += 1;
                    }
                }
            }
            '\0' => return false,
            _ => {
                lx.next();
                after_eq = false;
            }
        }
    }
}

impl Scanner for Julia {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let c = lx.lookahead();
        for (sym, ch) in [
            (IMMEDIATE_PAREN, '('),
            (IMMEDIATE_BRACKET, '['),
            (IMMEDIATE_BRACE, '{'),
            (IMMEDIATE_STRING_START, '"'),
            (IMMEDIATE_COMMAND_START, '`'),
        ] {
            if v[sym] && c == ch {
                lx.set_result(sym as u16);
                return true;
            }
        }
        if v[BLOCK_COMMENT_REST] && scan_block_comment(lx) {
            return true;
        }
        for (sym, ch, n, interp) in [
            (CONTENT_STR_1, '"', 1, true),
            (CONTENT_STR_3, '"', 3, true),
            (CONTENT_CMD_1, '`', 1, true),
            (CONTENT_CMD_3, '`', 3, true),
            (CONTENT_STR_1_RAW, '"', 1, false),
            (CONTENT_STR_3_RAW, '"', 3, false),
            (CONTENT_CMD_1_RAW, '`', 1, false),
            (CONTENT_CMD_3_RAW, '`', 3, false),
        ] {
            if v[sym] && scan_content(lx, sym, ch, n, interp) {
                return true;
            }
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
