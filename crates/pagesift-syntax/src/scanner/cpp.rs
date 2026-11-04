//! C++ 外部扫描器（原始字符串字面量），移植自 tree-sitter-cpp 的 `src/scanner.c`（MIT，Copyright (c) 2014 Max Brunsfeld）。

use super::{ScanLexer, Scanner, is_space};

const RAW_STRING_DELIMITER: usize = 0;
const RAW_STRING_CONTENT: usize = 1;
const MAX_DELIMITER_LENGTH: usize = 16;

#[derive(Default)]
pub struct Cpp {
    delimiter: Vec<u32>,
}

impl Cpp {
    fn scan_delimiter(&mut self, lx: &mut ScanLexer) -> bool {
        if !self.delimiter.is_empty() {
            // 结束分隔符必须与开头完全一致
            for i in 0..self.delimiter.len() {
                if lx.lookahead() as u32 != self.delimiter[i] {
                    return false;
                }
                lx.next();
            }
            self.delimiter.clear();
            return true;
        }
        loop {
            if self.delimiter.len() >= MAX_DELIMITER_LENGTH
                || lx.eof()
                || lx.lookahead() == '\\'
                || is_space(lx.lookahead())
            {
                return false;
            }
            if lx.lookahead() == '(' {
                return !self.delimiter.is_empty();
            }
            self.delimiter.push(lx.lookahead() as u32);
            lx.next();
        }
    }

    fn scan_content(&self, lx: &mut ScanLexer) -> bool {
        let mut idx: i32 = -1;
        loop {
            if lx.eof() {
                lx.mark_end();
                return true;
            }
            if idx >= 0 {
                if idx as usize == self.delimiter.len() {
                    if lx.lookahead() == '"' {
                        return true;
                    }
                    idx = -1;
                } else if lx.lookahead() as u32 == self.delimiter[idx as usize] {
                    idx += 1;
                } else {
                    idx = -1;
                }
            }
            if idx == -1 && lx.lookahead() == ')' {
                lx.mark_end();
                idx = 0;
            }
            lx.next();
        }
    }
}

impl Scanner for Cpp {
    fn serialize(&self, out: &mut Vec<u8>) {
        // 与官方一致：每个字符按 4 字节（wchar_t）小端存放
        for c in &self.delimiter {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    fn deserialize(&mut self, d: &[u8]) {
        self.delimiter.clear();
        if d.len().is_multiple_of(4) {
            self.delimiter.extend(
                d.as_chunks::<4>()
                    .0
                    .iter()
                    .take(MAX_DELIMITER_LENGTH)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            );
        }
    }

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        if valid[RAW_STRING_DELIMITER] && valid[RAW_STRING_CONTENT] {
            return false;
        }
        if valid[RAW_STRING_DELIMITER] {
            lx.set_result(RAW_STRING_DELIMITER as u16);
            return self.scan_delimiter(lx);
        }
        if valid[RAW_STRING_CONTENT] {
            lx.set_result(RAW_STRING_CONTENT as u16);
            return self.scan_content(lx);
        }
        false
    }
}
