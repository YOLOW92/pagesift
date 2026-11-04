//! Bash 外部扫描器，移植自 tree-sitter-bash 的 `src/scanner.c`（MIT，Copyright (c) 2017 Max Brunsfeld）。
//! 官方代码里的 `goto` 改成了分段函数 + 跳转枚举；here-doc 数组的复用方式（反序列化不截断）按原样保留。

mod front;
mod rest;

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_alpha, is_space};

const HEREDOC_START: usize = 0;
const SIMPLE_HEREDOC_BODY: usize = 1;
const HEREDOC_BODY_BEGINNING: usize = 2;
const HEREDOC_CONTENT: usize = 3;
const HEREDOC_END: usize = 4;
const FILE_DESCRIPTOR: usize = 5;
const EMPTY_VALUE: usize = 6;
const CONCAT: usize = 7;
const VARIABLE_NAME: usize = 8;
const TEST_OPERATOR: usize = 9;
const REGEX: usize = 10;
const REGEX_NO_SLASH: usize = 11;
const REGEX_NO_SPACE: usize = 12;
const EXPANSION_WORD: usize = 13;
const EXTGLOB_PATTERN: usize = 14;
const BARE_DOLLAR: usize = 15;
const BRACE_START: usize = 16;
const IMMEDIATE_DOUBLE_HASH: usize = 17;
const EXTERNAL_EXPANSION_SYM_HASH: usize = 18;
const EXTERNAL_EXPANSION_SYM_BANG: usize = 19;
const EXTERNAL_EXPANSION_SYM_EQUAL: usize = 20;
const CLOSING_BRACE: usize = 21;
const CLOSING_BRACKET: usize = 22;
const HEREDOC_ARROW: usize = 23;
const HEREDOC_ARROW_DASH: usize = 24;
const NEWLINE: usize = 25;
const OPENING_PAREN: usize = 26;
const ERROR_RECOVERY: usize = 28;

#[derive(Clone, Default)]
struct Heredoc {
    is_raw: bool,
    started: bool,
    allows_indent: bool,
    /// 与官方一样，`advance_word` 读出的分隔符末尾带一个 0 字节。
    delimiter: Vec<u8>,
    leading: Vec<u8>,
}

#[derive(Default)]
pub struct Bash {
    last_glob_paren_depth: u8,
    ext_was_in_double_quote: bool,
    ext_saw_outside_quote: bool,
    heredocs: Vec<Heredoc>,
}

enum St {
    Regex,
    Extglob,
    Expansion,
    Brace,
}

enum Nx {
    Ret(bool),
    Go(St),
}

fn ws(c: char) -> bool {
    is_space(c)
}
fn alpha(c: char) -> bool {
    is_alpha(c)
}
fn alnum(c: char) -> bool {
    c.is_alphanumeric()
}
fn low8(c: char) -> u8 {
    (c as u32 & 0xFF) as u8
}
fn cstr(v: &[u8]) -> &[u8] {
    let n = v.iter().position(|b| *b == 0).unwrap_or(v.len());
    &v[..n]
}

fn reset_heredoc(h: &mut Heredoc) {
    h.is_raw = false;
    h.started = false;
    h.allows_indent = false;
    h.delimiter.clear();
}

fn advance_word(lx: &mut ScanLexer, out: &mut Vec<u8>) -> bool {
    let mut empty = true;
    let mut quote = '\0';
    if lx.lookahead() == '\'' || lx.lookahead() == '"' {
        quote = lx.lookahead();
        lx.next();
    }
    while lx.lookahead() != '\0'
        && !(if quote != '\0' {
            lx.lookahead() == quote || lx.lookahead() == '\r' || lx.lookahead() == '\n'
        } else {
            ws(lx.lookahead())
        })
    {
        if lx.lookahead() == '\\' {
            lx.next();
            if lx.lookahead() == '\0' {
                return false;
            }
        }
        empty = false;
        out.push(low8(lx.lookahead()));
        lx.next();
    }
    out.push(0);
    if quote != '\0' && lx.lookahead() == quote {
        lx.next();
    }
    !empty
}

fn bare_dollar(lx: &mut ScanLexer) -> bool {
    while ws(lx.lookahead()) && lx.lookahead() != '\n' && !lx.eof() {
        lx.skip();
    }
    if lx.lookahead() == '$' {
        lx.next();
        lx.set_result(BARE_DOLLAR as u16);
        lx.mark_end();
        return ws(lx.lookahead()) || lx.eof() || lx.lookahead() == '"';
    }
    false
}

fn heredoc_start(h: &mut Heredoc, lx: &mut ScanLexer) -> bool {
    while ws(lx.lookahead()) {
        lx.skip();
    }
    lx.set_result(HEREDOC_START as u16);
    h.is_raw = matches!(lx.lookahead(), '\'' | '"' | '\\');
    let found = advance_word(lx, &mut h.delimiter);
    if !found {
        h.delimiter.clear();
        return false;
    }
    found
}

fn heredoc_end_identifier(h: &mut Heredoc, lx: &mut ScanLexer) -> bool {
    h.leading.clear();
    let mut size = 0usize;
    if !h.delimiter.is_empty() {
        while lx.lookahead() != '\0'
            && lx.lookahead() != '\n'
            && size < h.delimiter.len()
            && (h.delimiter[size] as i8 as i32) == lx.lookahead() as i32
            && h.leading.len() < h.delimiter.len()
        {
            h.leading.push(low8(lx.lookahead()));
            lx.next();
            size += 1;
        }
    }
    h.leading.push(0);
    if h.delimiter.is_empty() {
        false
    } else {
        cstr(&h.leading) == cstr(&h.delimiter)
    }
}

impl Bash {
    fn heredoc_content(&mut self, lx: &mut ScanLexer, middle: usize, end: usize) -> bool {
        let mut did_advance = false;
        let last = self.heredocs.len() - 1;
        loop {
            match lx.lookahead() {
                '\0' => {
                    if lx.eof() && did_advance {
                        reset_heredoc(&mut self.heredocs[last]);
                        lx.set_result(end as u16);
                        return true;
                    }
                    return false;
                }
                '\\' => {
                    did_advance = true;
                    lx.next();
                    lx.next();
                }
                '$' => {
                    if self.heredocs[last].is_raw {
                        did_advance = true;
                        lx.next();
                        continue;
                    }
                    if did_advance {
                        lx.mark_end();
                        lx.set_result(middle as u16);
                        self.heredocs[last].started = true;
                        lx.next();
                        if alpha(lx.lookahead()) || lx.lookahead() == '{' || lx.lookahead() == '(' {
                            return true;
                        }
                        continue;
                    }
                    if middle == HEREDOC_BODY_BEGINNING && lx.column() == 0 {
                        lx.set_result(middle as u16);
                        self.heredocs[last].started = true;
                        return true;
                    }
                    return false;
                }
                '\n' => {
                    if !did_advance {
                        lx.skip();
                    } else {
                        lx.next();
                    }
                    did_advance = true;
                    if self.heredocs[last].allows_indent {
                        while ws(lx.lookahead()) {
                            lx.next();
                        }
                    }
                    lx.set_result(if self.heredocs[last].started {
                        middle
                    } else {
                        end
                    } as u16);
                    lx.mark_end();
                    if heredoc_end_identifier(&mut self.heredocs[last], lx) {
                        if lx.result as usize == HEREDOC_END {
                            self.heredocs.pop();
                        }
                        return true;
                    }
                }
                _ => {
                    if lx.column() == 0 {
                        while ws(lx.lookahead()) {
                            if did_advance {
                                lx.next();
                            } else {
                                lx.skip();
                            }
                        }
                        if end != SIMPLE_HEREDOC_BODY {
                            lx.set_result(middle as u16);
                            if heredoc_end_identifier(&mut self.heredocs[last], lx) {
                                return true;
                            }
                        }
                        if end == SIMPLE_HEREDOC_BODY {
                            lx.set_result(end as u16);
                            lx.mark_end();
                            if heredoc_end_identifier(&mut self.heredocs[last], lx) {
                                return true;
                            }
                        }
                    }
                    did_advance = true;
                    lx.next();
                }
            }
        }
    }
}

impl Scanner for Bash {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.last_glob_paren_depth);
        out.push(self.ext_was_in_double_quote as u8);
        out.push(self.ext_saw_outside_quote as u8);
        out.push(self.heredocs.len() as u8);
        for h in &self.heredocs {
            if out.len() + 3 + 4 + h.delimiter.len() >= MAX_SERIALIZED {
                out.clear();
                return;
            }
            out.push(h.is_raw as u8);
            out.push(h.started as u8);
            out.push(h.allows_indent as u8);
            out.extend_from_slice(&(h.delimiter.len() as u32).to_le_bytes());
            out.extend_from_slice(&h.delimiter);
        }
    }

    /// 与官方一致：长度为 0 只清空已有的 here-doc 项（不删除），否则逐项覆盖、不截断多余项。
    fn deserialize(&mut self, d: &[u8]) {
        if d.is_empty() {
            for h in &mut self.heredocs {
                reset_heredoc(h);
            }
            return;
        }
        if d.len() < 4 {
            return;
        }
        self.last_glob_paren_depth = d[0];
        self.ext_was_in_double_quote = d[1] != 0;
        self.ext_saw_outside_quote = d[2] != 0;
        let count = d[3] as usize;
        let mut p = 4;
        for i in 0..count {
            if p + 7 > d.len() {
                break;
            }
            if i >= self.heredocs.len() {
                self.heredocs.push(Heredoc::default());
            }
            let h = &mut self.heredocs[i];
            h.is_raw = d[p] != 0;
            h.started = d[p + 1] != 0;
            h.allows_indent = d[p + 2] != 0;
            let n = u32::from_le_bytes([d[p + 3], d[p + 4], d[p + 5], d[p + 6]]) as usize;
            p += 7;
            let n = n.min(d.len() - p);
            h.delimiter = d[p..p + n].to_vec();
            p += n;
        }
    }

    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let mut st = match self.front(lx, v) {
            Nx::Ret(b) => return b,
            Nx::Go(s) => s,
        };
        loop {
            let nx = match st {
                St::Regex => self.regex(lx, v),
                St::Extglob => self.extglob(lx, v),
                St::Expansion => self.expansion_word(lx, v),
                St::Brace => return self.brace_start(lx, v),
            };
            match nx {
                Nx::Ret(b) => return b,
                Nx::Go(s) => st = s,
            }
        }
    }
}
