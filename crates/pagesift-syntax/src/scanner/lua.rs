//! Lua 外部扫描器，移植自 tree-sitter-lua 的 `src/scanner.c`（MIT，Copyright (c) 2021 Munif Tanjim）。

use super::{ScanLexer, Scanner, is_space};

const BLOCK_COMMENT_START: usize = 0;
const BLOCK_COMMENT_CONTENT: usize = 1;
const BLOCK_COMMENT_END: usize = 2;
const BLOCK_STRING_START: usize = 3;
const BLOCK_STRING_CONTENT: usize = 4;
const BLOCK_STRING_END: usize = 5;

#[derive(Default)]
pub struct Lua {
    ending_char: u8,
    level_count: u8,
}

fn consume_char(c: char, lx: &mut ScanLexer) -> bool {
    if lx.lookahead() != c {
        return false;
    }
    lx.next();
    true
}

fn count_char(c: char, lx: &mut ScanLexer) -> u8 {
    let mut n: u8 = 0;
    while lx.lookahead() == c {
        n = n.wrapping_add(1);
        lx.next();
    }
    n
}

impl Lua {
    fn block_start(&mut self, lx: &mut ScanLexer) -> bool {
        if consume_char('[', lx) {
            let level = count_char('=', lx);
            if consume_char('[', lx) {
                self.level_count = level;
                return true;
            }
        }
        false
    }

    fn block_end(&self, lx: &mut ScanLexer) -> bool {
        if consume_char(']', lx) {
            let level = count_char('=', lx);
            if self.level_count == level && consume_char(']', lx) {
                return true;
            }
        }
        false
    }

    fn block_content(&self, lx: &mut ScanLexer) -> bool {
        while lx.lookahead() != '\0' {
            if lx.lookahead() == ']' {
                lx.mark_end();
                if self.block_end(lx) {
                    return true;
                }
            } else {
                lx.next();
            }
        }
        false
    }

    fn comment_start(&mut self, lx: &mut ScanLexer) -> bool {
        if consume_char('-', lx) && consume_char('-', lx) {
            lx.mark_end();
            if self.block_start(lx) {
                lx.mark_end();
                lx.set_result(BLOCK_COMMENT_START as u16);
                return true;
            }
        }
        false
    }

    fn comment_content(&mut self, lx: &mut ScanLexer) -> bool {
        if self.ending_char == 0 {
            if self.block_content(lx) {
                lx.set_result(BLOCK_COMMENT_CONTENT as u16);
                return true;
            }
            return false;
        }
        while lx.lookahead() != '\0' {
            if lx.lookahead() as u32 == self.ending_char as u32 {
                self.ending_char = 0;
                self.level_count = 0;
                lx.set_result(BLOCK_COMMENT_CONTENT as u16);
                return true;
            }
            lx.next();
        }
        false
    }
}

impl Scanner for Lua {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.ending_char);
        out.push(self.level_count);
    }
    /// 与官方一致：长度为 0 时不重置，保留上一次的状态。
    fn deserialize(&mut self, d: &[u8]) {
        if d.is_empty() {
            return;
        }
        self.ending_char = d[0];
        if d.len() > 1 {
            self.level_count = d[1];
        }
    }

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        if valid[BLOCK_STRING_END] && self.block_end(lx) {
            self.ending_char = 0;
            self.level_count = 0;
            lx.set_result(BLOCK_STRING_END as u16);
            return true;
        }
        if valid[BLOCK_STRING_CONTENT] && self.block_content(lx) {
            lx.set_result(BLOCK_STRING_CONTENT as u16);
            return true;
        }
        if valid[BLOCK_COMMENT_END] && self.ending_char == 0 && self.block_end(lx) {
            self.ending_char = 0;
            self.level_count = 0;
            lx.set_result(BLOCK_COMMENT_END as u16);
            return true;
        }
        if valid[BLOCK_COMMENT_CONTENT] && self.comment_content(lx) {
            return true;
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if valid[BLOCK_STRING_START] && self.block_start(lx) {
            lx.set_result(BLOCK_STRING_START as u16);
            return true;
        }
        if valid[BLOCK_COMMENT_START] && self.comment_start(lx) {
            return true;
        }
        false
    }
}
