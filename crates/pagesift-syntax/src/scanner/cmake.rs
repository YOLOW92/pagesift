//! CMake 外部扫描器，移植自 tree-sitter-cmake 的 `src/scanner.c`（MIT，Copyright (c) 2019 Uy Ha）。

use super::{ScanLexer, Scanner, is_space};

const BRACKET_ARGUMENT_OPEN: u32 = 0;
const BRACKET_ARGUMENT_CONTENT: u32 = 1;
const BRACKET_ARGUMENT_CLOSE: u32 = 2;
const BRACKET_COMMENT_OPEN: u32 = 3;
const BRACKET_COMMENT_CONTENT: u32 = 4;
const BRACKET_COMMENT_CLOSE: u32 = 5;
const LINE_COMMENT: u32 = 6;

/// 状态是（括号层级, 上一个记号），共 8 字节；初始记号为 0，与官方的 calloc 一致。
#[derive(Default)]
pub struct CMake {
    level: u32,
    token: u32,
}

impl CMake {
    fn open_brackets(&mut self, lx: &mut ScanLexer) -> bool {
        if lx.lookahead() != '[' {
            return false;
        }
        lx.next();
        let mut level = 0;
        while lx.lookahead() == '=' {
            level += 1;
            lx.next();
        }
        if lx.lookahead() != '[' {
            return false;
        }
        lx.next();
        lx.mark_end();
        self.level = level;
        true
    }

    fn bracketed_content(&self, lx: &mut ScanLexer) {
        while !lx.eof() {
            if lx.lookahead() == ']' {
                let mut level = 0;
                lx.mark_end();
                lx.next();
                while lx.lookahead() == '=' {
                    level += 1;
                    lx.next();
                }
                if level == self.level && lx.lookahead() == ']' {
                    break;
                }
                continue;
            }
            lx.next();
            lx.mark_end();
        }
    }

    fn close_brackets(&mut self, lx: &mut ScanLexer) -> bool {
        if lx.lookahead() != ']' {
            return false;
        }
        let mut level = 0;
        lx.next();
        while lx.lookahead() == '=' {
            level += 1;
            lx.next();
        }
        if level != self.level || lx.lookahead() != ']' {
            return false;
        }
        lx.next();
        lx.mark_end();
        self.level = 0;
        true
    }

    fn set(&mut self, lx: &mut ScanLexer, token: u32) {
        self.token = token;
        lx.set_result(token as u16);
    }
}

impl Scanner for CMake {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if v[BRACKET_ARGUMENT_OPEN as usize] && self.open_brackets(lx) {
            self.set(lx, BRACKET_ARGUMENT_OPEN);
            return true;
        }
        if v[BRACKET_ARGUMENT_CONTENT as usize] && self.token == BRACKET_ARGUMENT_OPEN {
            self.bracketed_content(lx);
            self.set(lx, BRACKET_ARGUMENT_CONTENT);
            return true;
        }
        if v[BRACKET_ARGUMENT_CLOSE as usize]
            && self.token == BRACKET_ARGUMENT_CONTENT
            && self.close_brackets(lx)
        {
            lx.set_result(BRACKET_ARGUMENT_CLOSE as u16);
            return true;
        }
        if lx.lookahead() == '#' {
            if !v[BRACKET_COMMENT_OPEN as usize] && !v[LINE_COMMENT as usize] {
                return false;
            }
            lx.next();
            if self.open_brackets(lx) {
                self.set(lx, BRACKET_COMMENT_OPEN);
                return true;
            }
            while !matches!(lx.lookahead(), '\r' | '\n' | '\0') {
                lx.next();
            }
            lx.mark_end();
            lx.set_result(LINE_COMMENT as u16);
            return true;
        }
        if v[BRACKET_COMMENT_CONTENT as usize] && self.token == BRACKET_COMMENT_OPEN {
            self.bracketed_content(lx);
            self.set(lx, BRACKET_COMMENT_CONTENT);
            return true;
        }
        if v[BRACKET_COMMENT_CLOSE as usize]
            && self.token == BRACKET_COMMENT_CONTENT
            && self.close_brackets(lx)
        {
            lx.set_result(BRACKET_COMMENT_CLOSE as u16);
            return true;
        }
        false
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.level.to_le_bytes());
        out.extend_from_slice(&self.token.to_le_bytes());
    }

    fn deserialize(&mut self, d: &[u8]) {
        if d.len() == 8 {
            self.level = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
            self.token = u32::from_le_bytes([d[4], d[5], d[6], d[7]]);
        } else {
            self.level = 0;
        }
    }
}
