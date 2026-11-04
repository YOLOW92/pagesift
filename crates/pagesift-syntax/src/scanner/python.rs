//! Python 外部扫描器，移植自 tree-sitter-python 的 `src/scanner.c`（MIT，Copyright (c) 2016 Max Brunsfeld）。

use super::{MAX_SERIALIZED, ScanLexer, Scanner};

const NEWLINE: usize = 0;
const INDENT: usize = 1;
const DEDENT: usize = 2;
const STRING_START: usize = 3;
const STRING_CONTENT: usize = 4;
const ESCAPE_INTERPOLATION: usize = 5;
const STRING_END: usize = 6;
const COMMENT_UNUSED: usize = 7;
const CLOSE_PAREN: usize = 8;
const CLOSE_BRACKET: usize = 9;
const CLOSE_BRACE: usize = 10;
const EXCEPT: usize = 11;

const SINGLE: u8 = 1;
const DOUBLE: u8 = 2;
const BACK: u8 = 4;
const RAW: u8 = 8;
const FORMAT: u8 = 16;
const TRIPLE: u8 = 32;
const BYTES: u8 = 64;

fn end_char(f: u8) -> char {
    if f & SINGLE != 0 {
        '\''
    } else if f & DOUBLE != 0 {
        '"'
    } else if f & BACK != 0 {
        '`'
    } else {
        '\0'
    }
}

pub struct Python {
    indents: Vec<u16>,
    delimiters: Vec<u8>,
    inside_interpolated: bool,
}

impl Default for Python {
    fn default() -> Self {
        Python {
            indents: vec![0],
            delimiters: Vec::new(),
            inside_interpolated: false,
        }
    }
}

impl Scanner for Python {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.inside_interpolated as u8);
        let n = self.delimiters.len().min(255);
        out.push(n as u8);
        out.extend_from_slice(&self.delimiters[..n]);
        for &v in self.indents.iter().skip(1) {
            if out.len() >= MAX_SERIALIZED {
                break;
            }
            out.push((v & 0xFF) as u8);
            out.push((v >> 8) as u8);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.delimiters.clear();
        self.indents.clear();
        self.indents.push(0);
        self.inside_interpolated = false;
        if d.len() > 1 {
            self.inside_interpolated = d[0] != 0;
            let n = d[1] as usize;
            let mut p = 2;
            if n > 0 && p + n <= d.len() {
                self.delimiters.extend_from_slice(&d[p..p + n]);
                p += n;
            }
            while p + 1 < d.len() {
                self.indents.push(d[p] as u16 | (d[p + 1] as u16) << 8);
                p += 2;
            }
        }
    }

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        let error_recovery = valid[STRING_CONTENT] && valid[INDENT];
        let within_brackets = valid[CLOSE_BRACE] || valid[CLOSE_PAREN] || valid[CLOSE_BRACKET];
        let advanced_once = false;
        if valid[ESCAPE_INTERPOLATION]
            && !self.delimiters.is_empty()
            && matches!(lx.lookahead(), '{' | '}')
            && !error_recovery
        {
            let d = *self.delimiters.last().unwrap();
            if d & FORMAT != 0 {
                lx.mark_end();
                let left = lx.lookahead() == '{';
                lx.next();
                if (lx.lookahead() == '{' && left) || (lx.lookahead() == '}' && !left) {
                    lx.next();
                    lx.mark_end();
                    lx.set_result(ESCAPE_INTERPOLATION as u16);
                    return true;
                }
                return false;
            }
        }
        if valid[STRING_CONTENT] && !self.delimiters.is_empty() && !error_recovery {
            let d = *self.delimiters.last().unwrap();
            let ec = end_char(d);
            let mut has_content = advanced_once;
            while lx.lookahead() != '\0' {
                let c = lx.lookahead();
                if (advanced_once || c == '{' || c == '}') && d & FORMAT != 0 {
                    lx.mark_end();
                    lx.set_result(STRING_CONTENT as u16);
                    return has_content;
                }
                if c == '\\' {
                    if d & RAW != 0 {
                        lx.next();
                        if lx.lookahead() == ec || lx.lookahead() == '\\' {
                            lx.next();
                        }
                        if lx.lookahead() == '\r' {
                            lx.next();
                            if lx.lookahead() == '\n' {
                                lx.next();
                            }
                        } else if lx.lookahead() == '\n' {
                            lx.next();
                        }
                        continue;
                    }
                    if d & BYTES != 0 {
                        lx.mark_end();
                        lx.next();
                        if matches!(lx.lookahead(), 'N' | 'u' | 'U') {
                            lx.next();
                        } else {
                            lx.set_result(STRING_CONTENT as u16);
                            return has_content;
                        }
                    } else {
                        lx.mark_end();
                        lx.set_result(STRING_CONTENT as u16);
                        return has_content;
                    }
                } else if c == ec {
                    if d & TRIPLE != 0 {
                        lx.mark_end();
                        lx.next();
                        if lx.lookahead() == ec {
                            lx.next();
                            if lx.lookahead() == ec {
                                if has_content {
                                    lx.set_result(STRING_CONTENT as u16);
                                } else {
                                    lx.next();
                                    lx.mark_end();
                                    self.delimiters.pop();
                                    lx.set_result(STRING_END as u16);
                                    self.inside_interpolated = false;
                                }
                                return true;
                            }
                            lx.mark_end();
                            lx.set_result(STRING_CONTENT as u16);
                            return true;
                        }
                        lx.mark_end();
                        lx.set_result(STRING_CONTENT as u16);
                        return true;
                    }
                    if has_content {
                        lx.set_result(STRING_CONTENT as u16);
                    } else {
                        lx.next();
                        self.delimiters.pop();
                        lx.set_result(STRING_END as u16);
                        self.inside_interpolated = false;
                    }
                    lx.mark_end();
                    return true;
                } else if c == '\n' && has_content && d & TRIPLE == 0 {
                    return false;
                }
                lx.next();
                has_content = true;
            }
        }

        lx.mark_end();
        let mut found_eol = false;
        let mut indent: u16 = 0;
        let mut first_comment_indent: i32 = -1;
        loop {
            let c = lx.lookahead();
            if c == '\n' {
                found_eol = true;
                indent = 0;
                lx.skip();
            } else if c == ' ' {
                indent = indent.wrapping_add(1);
                lx.skip();
            } else if c == '\r' || c == '\u{c}' {
                indent = 0;
                lx.skip();
            } else if c == '\t' {
                indent = indent.wrapping_add(8);
                lx.skip();
            } else if c == '#'
                && (valid[INDENT] || valid[DEDENT] || valid[NEWLINE] || valid[EXCEPT])
            {
                if !found_eol {
                    return false;
                }
                if first_comment_indent == -1 {
                    first_comment_indent = indent as i32;
                }
                while lx.lookahead() != '\0' && lx.lookahead() != '\n' {
                    lx.skip();
                }
                lx.skip();
                indent = 0;
            } else if c == '\\' {
                lx.skip();
                if lx.lookahead() == '\r' {
                    lx.skip();
                }
                if lx.lookahead() == '\n' || lx.eof() {
                    lx.skip();
                } else {
                    return false;
                }
            } else if lx.eof() {
                indent = 0;
                found_eol = true;
                break;
            } else {
                break;
            }
        }

        if found_eol {
            if !self.indents.is_empty() {
                let cur = *self.indents.last().unwrap();
                if valid[INDENT] && indent > cur {
                    self.indents.push(indent);
                    lx.set_result(INDENT as u16);
                    return true;
                }
                let next_is_string = matches!(lx.lookahead(), '"' | '\'' | '`');
                if (valid[DEDENT]
                    || (!valid[NEWLINE]
                        && !(valid[STRING_START] && next_is_string)
                        && !within_brackets))
                    && indent < cur
                    && !self.inside_interpolated
                    && first_comment_indent < cur as i32
                {
                    self.indents.pop();
                    lx.set_result(DEDENT as u16);
                    return true;
                }
            }
            if valid[NEWLINE] && !error_recovery {
                lx.set_result(NEWLINE as u16);
                return true;
            }
        }

        if first_comment_indent == -1 && valid[STRING_START] {
            let mut d: u8 = 0;
            let mut has_flags = false;
            while lx.lookahead() != '\0' {
                match lx.lookahead() {
                    'f' | 'F' | 't' | 'T' => d |= FORMAT,
                    'r' | 'R' => d |= RAW,
                    'b' | 'B' => d |= BYTES,
                    'u' | 'U' => {}
                    _ => break,
                }
                has_flags = true;
                lx.next();
            }
            match lx.lookahead() {
                '`' => {
                    d |= BACK;
                    lx.next();
                    lx.mark_end();
                }
                q @ ('\'' | '"') => {
                    d |= if q == '\'' { SINGLE } else { DOUBLE };
                    lx.next();
                    lx.mark_end();
                    if lx.lookahead() == q {
                        lx.next();
                        if lx.lookahead() == q {
                            lx.next();
                            lx.mark_end();
                            d |= TRIPLE;
                        }
                    }
                }
                _ => {}
            }
            if end_char(d) != '\0' {
                self.delimiters.push(d);
                lx.set_result(STRING_START as u16);
                self.inside_interpolated = d & FORMAT != 0;
                return true;
            }
            if has_flags {
                return false;
            }
        }
        let _ = COMMENT_UNUSED;
        false
    }
}
