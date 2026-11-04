//! Dart 外部扫描器，移植自 tree-sitter-dart 的 `src/scanner.c`（MIT，Copyright (c) 2020-2023 UserNobody14 and others）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner, is_space};

const TEMPLATE_CHARS_SINGLE: usize = 0;
const TEMPLATE_CHARS_DOUBLE: usize = 1;
const TEMPLATE_CHARS_SINGLE_SINGLE: usize = 2;
const TEMPLATE_CHARS_DOUBLE_SINGLE: usize = 3;
const TEMPLATE_CHARS_RAW_SLASH: usize = 4;
const BLOCK_COMMENT: usize = 5;
const DOCUMENTATION_BLOCK_COMMENT: usize = 6;

#[derive(Default)]
pub struct Dart;

fn scan_multiline_comments(lx: &mut ScanLexer) -> bool {
    lx.next();
    if lx.lookahead() != '*' {
        return false;
    }
    lx.next();
    let doc = lx.lookahead() == '*';
    let mut after_star = false;
    let mut depth = 1u32;
    loop {
        match lx.lookahead() {
            '\0' => return false,
            '*' => {
                lx.next();
                after_star = true;
            }
            '/' => {
                if after_star {
                    lx.next();
                    after_star = false;
                    depth -= 1;
                    if depth == 0 {
                        let sym = if doc {
                            DOCUMENTATION_BLOCK_COMMENT
                        } else {
                            BLOCK_COMMENT
                        };
                        lx.set_result(sym as u16);
                        return true;
                    }
                } else {
                    lx.next();
                    after_star = false;
                    if lx.lookahead() == '*' {
                        depth += 1;
                        lx.next();
                    }
                }
            }
            _ => {
                lx.next();
                after_star = false;
            }
        }
    }
}

fn scan_templates(lx: &mut ScanLexer, v: &[bool]) -> bool {
    let sym = if v[TEMPLATE_CHARS_DOUBLE] {
        TEMPLATE_CHARS_DOUBLE
    } else if v[TEMPLATE_CHARS_SINGLE] {
        TEMPLATE_CHARS_SINGLE
    } else if v[TEMPLATE_CHARS_SINGLE_SINGLE] {
        TEMPLATE_CHARS_SINGLE_SINGLE
    } else {
        TEMPLATE_CHARS_DOUBLE_SINGLE
    };
    lx.set_result(sym as u16);
    let mut has_content = false;
    loop {
        lx.mark_end();
        match lx.lookahead() {
            '\'' | '"' | '$' => return has_content,
            '\n' => {
                if v[TEMPLATE_CHARS_DOUBLE_SINGLE] || v[TEMPLATE_CHARS_SINGLE_SINGLE] {
                    return false;
                }
                lx.next();
            }
            '\0' => return false,
            '\\' => {
                if v[TEMPLATE_CHARS_RAW_SLASH] {
                    lx.set_result(TEMPLATE_CHARS_RAW_SLASH as u16);
                    lx.next();
                } else {
                    return has_content;
                }
            }
            _ => lx.next(),
        }
        has_content = true;
    }
}

impl Scanner for Dart {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if v[TEMPLATE_CHARS_DOUBLE]
            || v[TEMPLATE_CHARS_SINGLE]
            || v[TEMPLATE_CHARS_DOUBLE_SINGLE]
            || v[TEMPLATE_CHARS_SINGLE_SINGLE]
        {
            return scan_templates(lx, v);
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if lx.lookahead() == '/' {
            return scan_multiline_comments(lx);
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
