//! PHP 外部扫描器，移植自 tree-sitter-php 的 `common/scanner.h`（MIT，Copyright (c) 2017 Josh Vera, GitHub）。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_space};

const AUTOMATIC_SEMICOLON: usize = 0;
const ENCAPSED_STRING_CHARS: usize = 1;
const ENCAPSED_STRING_CHARS_AFTER_VARIABLE: usize = 2;
const EXECUTION_STRING_CHARS: usize = 3;
const EXECUTION_STRING_CHARS_AFTER_VARIABLE: usize = 4;
const ENCAPSED_STRING_CHARS_HEREDOC: usize = 5;
const ENCAPSED_STRING_CHARS_AFTER_VARIABLE_HEREDOC: usize = 6;
const EOF_TOKEN: usize = 7;
const HEREDOC_START: usize = 8;
const HEREDOC_END: usize = 9;
const NOWDOC_STRING: usize = 10;
const SENTINEL_ERROR: usize = 11;

#[derive(Default)]
pub struct Php {
    heredocs: Vec<(bool, Vec<char>)>,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c as u32 >= 0x80
}

fn is_escapable(lx: &mut ScanLexer) -> bool {
    let c = lx.lookahead();
    if matches!(c, 'n' | 'r' | 't' | 'v' | 'e' | 'f' | '\\' | '$' | '"') {
        return true;
    }
    if c == 'x' {
        lx.next();
        return lx.lookahead().is_ascii_hexdigit();
    }
    if c == 'u' {
        return true;
    }
    matches!(lx.lookahead(), '0'..='7')
}

fn heredoc_word(lx: &mut ScanLexer) -> Vec<char> {
    let mut w = Vec::new();
    while is_name_char(lx.lookahead()) {
        w.push(lx.lookahead());
        lx.next();
    }
    w
}

fn whitespace(lx: &mut ScanLexer) -> bool {
    loop {
        while is_space(lx.lookahead()) {
            lx.next();
        }
        if lx.lookahead() == '/' {
            lx.next();
            if lx.lookahead() == '/' {
                lx.next();
                while lx.lookahead() != '\0' && lx.lookahead() != '\n' {
                    lx.next();
                }
            } else {
                return false;
            }
        } else {
            return true;
        }
    }
}

impl Php {
    fn nowdoc(&self, lx: &mut ScanLexer) -> bool {
        let mut consumed = false;
        let Some((_, tag)) = self.heredocs.last() else {
            return false;
        };
        while is_space(lx.lookahead()) {
            lx.next();
            consumed = true;
        }
        let mut matched = false;
        for (i, &c) in tag.iter().enumerate() {
            if lx.lookahead() != c {
                break;
            }
            lx.next();
            consumed = true;
            matched = i == tag.len() - 1 && !is_name_char(lx.lookahead());
        }
        if matched {
            return false;
        }
        let mut has_content = consumed;
        loop {
            lx.mark_end();
            match lx.lookahead() {
                '\n' | '\r' => return has_content,
                _ => {
                    if lx.eof() {
                        return false;
                    }
                    lx.next();
                }
            }
            has_content = true;
        }
    }

    fn encapsed(&self, lx: &mut ScanLexer, mut after_var: bool, heredoc: bool, exec: bool) -> bool {
        let mut consumed = false;
        if heredoc && !self.heredocs.is_empty() {
            while is_space(lx.lookahead()) && lx.lookahead() != '\r' && lx.lookahead() != '\n' {
                lx.next();
                consumed = true;
            }
            let tag = &self.heredocs.last().unwrap().1;
            let mut matched = false;
            for (i, &c) in tag.iter().enumerate() {
                if lx.lookahead() != c {
                    break;
                }
                consumed = true;
                lx.next();
                matched = i == tag.len() - 1 && !is_name_char(lx.lookahead());
            }
            if matched {
                return false;
            }
        }
        let mut has_content = consumed;
        loop {
            lx.mark_end();
            match lx.lookahead() {
                '"' => {
                    if !heredoc && !exec {
                        return has_content;
                    }
                    lx.next();
                }
                '`' => {
                    if exec {
                        return has_content;
                    }
                    lx.next();
                }
                '\n' | '\r' => {
                    if heredoc {
                        return has_content;
                    }
                    lx.next();
                }
                '\\' => {
                    lx.next();
                    if lx.lookahead() == '{' {
                        lx.next();
                    } else if exec && lx.lookahead() == '`' {
                        return has_content;
                    } else if heredoc && lx.lookahead() == '\\' {
                        lx.next();
                    } else if is_escapable(lx) {
                        return has_content;
                    }
                }
                '$' => {
                    lx.next();
                    if (is_name_char(lx.lookahead()) && !lx.lookahead().is_ascii_digit())
                        || lx.lookahead() == '{'
                    {
                        return has_content;
                    }
                }
                '-' if after_var => {
                    lx.next();
                    if lx.lookahead() == '>' {
                        lx.next();
                        if is_name_char(lx.lookahead()) {
                            return has_content;
                        }
                    }
                }
                // C 里 `-` 在非变量后落入 `[` 分支
                '-' | '[' => {
                    if after_var && lx.lookahead() == '[' {
                        return has_content;
                    }
                    lx.next();
                }
                '{' => {
                    lx.next();
                    if lx.lookahead() == '$' {
                        return has_content;
                    }
                }
                _ => {
                    if lx.eof() {
                        return false;
                    }
                    lx.next();
                }
            }
            has_content = true;
            after_var = false;
        }
    }
}

impl Scanner for Php {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.heredocs.len() as u8);
        for (indent, word) in &self.heredocs {
            if out.len() + 5 + word.len() * 4 >= MAX_SERIALIZED {
                out.clear();
                return;
            }
            out.push(*indent as u8);
            out.extend_from_slice(&(word.len() as u32).to_le_bytes());
            for c in word {
                out.extend_from_slice(&(*c as u32).to_le_bytes());
            }
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.heredocs.clear();
        if d.is_empty() {
            return;
        }
        let n = d[0] as usize;
        let mut p = 1;
        for _ in 0..n {
            if p + 5 > d.len() {
                break;
            }
            let indent = d[p] != 0;
            let len = u32::from_le_bytes([d[p + 1], d[p + 2], d[p + 3], d[p + 4]]) as usize;
            p += 5;
            let mut word = Vec::new();
            for _ in 0..len {
                if p + 4 > d.len() {
                    break;
                }
                word.push(
                    char::from_u32(u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]))
                        .unwrap_or('\u{FFFD}'),
                );
                p += 4;
            }
            self.heredocs.push((indent, word));
        }
    }

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        if valid[SENTINEL_ERROR] {
            return false;
        }
        lx.mark_end();
        let enc = [
            (ENCAPSED_STRING_CHARS_AFTER_VARIABLE, true, false, false),
            (ENCAPSED_STRING_CHARS, false, false, false),
            (EXECUTION_STRING_CHARS_AFTER_VARIABLE, true, false, true),
            (EXECUTION_STRING_CHARS, false, false, true),
            (
                ENCAPSED_STRING_CHARS_AFTER_VARIABLE_HEREDOC,
                true,
                true,
                false,
            ),
            (ENCAPSED_STRING_CHARS_HEREDOC, false, true, false),
        ];
        for (sym, after, heredoc, exec) in enc {
            if valid[sym] {
                lx.set_result(sym as u16);
                return self.encapsed(lx, after, heredoc, exec);
            }
        }
        if valid[NOWDOC_STRING] {
            lx.set_result(NOWDOC_STRING as u16);
            return self.nowdoc(lx);
        }
        if valid[HEREDOC_END] {
            lx.set_result(HEREDOC_END as u16);
            let Some((_, tag)) = self.heredocs.last() else {
                return false;
            };
            let tag = tag.clone();
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            if heredoc_word(lx) != tag {
                return false;
            }
            lx.mark_end();
            self.heredocs.pop();
            return true;
        }
        if !whitespace(lx) {
            return false;
        }
        if valid[EOF_TOKEN] && lx.eof() {
            lx.set_result(EOF_TOKEN as u16);
            return true;
        }
        if valid[HEREDOC_START] {
            lx.set_result(HEREDOC_START as u16);
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            let word = heredoc_word(lx);
            if word.is_empty() {
                return false;
            }
            lx.mark_end();
            self.heredocs.push((false, word));
            return true;
        }
        if valid[AUTOMATIC_SEMICOLON] {
            lx.set_result(AUTOMATIC_SEMICOLON as u16);
            if lx.lookahead() != '?' {
                return false;
            }
            lx.next();
            return lx.lookahead() == '>';
        }
        false
    }
}
