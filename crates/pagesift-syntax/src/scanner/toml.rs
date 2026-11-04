//! TOML 外部扫描器，移植自 tree-sitter-toml 的 `src/scanner.c`（MIT，Copyright (c) Ika）。

use super::{ScanLexer, Scanner};

const LINE_ENDING_OR_EOF: usize = 0;
const MULTILINE_BASIC_STRING_CONTENT: usize = 1;
const MULTILINE_BASIC_STRING_END: usize = 2;
const MULTILINE_LITERAL_STRING_CONTENT: usize = 3;
const MULTILINE_LITERAL_STRING_END: usize = 4;

pub struct Toml;

fn multiline_string_end(
    lx: &mut ScanLexer,
    valid: &[bool],
    delim: char,
    content: usize,
    end: usize,
) -> bool {
    if !valid[end] || lx.lookahead() != delim {
        return false;
    }
    lx.next();
    lx.mark_end();
    if lx.lookahead() != delim {
        lx.set_result(content as u16);
        return true;
    }
    lx.next();
    if lx.lookahead() != delim {
        lx.mark_end();
        lx.set_result(content as u16);
        return true;
    }
    lx.next();
    if lx.lookahead() != delim {
        lx.mark_end();
        lx.set_result(end as u16);
        return true;
    }
    lx.set_result(content as u16);
    true
}

impl Scanner for Toml {
    fn serialize(&self, _out: &mut Vec<u8>) {}
    fn deserialize(&mut self, _d: &[u8]) {}

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        if multiline_string_end(
            lx,
            valid,
            '"',
            MULTILINE_BASIC_STRING_CONTENT,
            MULTILINE_BASIC_STRING_END,
        ) || multiline_string_end(
            lx,
            valid,
            '\'',
            MULTILINE_LITERAL_STRING_CONTENT,
            MULTILINE_LITERAL_STRING_END,
        ) {
            return true;
        }
        if valid[LINE_ENDING_OR_EOF] {
            lx.set_result(LINE_ENDING_OR_EOF as u16);
            while lx.lookahead() == ' ' || lx.lookahead() == '\t' {
                lx.skip();
            }
            if lx.lookahead() == '\0' || lx.lookahead() == '\n' {
                return true;
            }
            if lx.lookahead() == '\r' {
                lx.skip();
                if lx.lookahead() == '\n' {
                    return true;
                }
            }
        }
        false
    }
}
