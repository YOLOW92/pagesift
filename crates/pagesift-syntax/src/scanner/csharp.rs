//! C# 外部扫描器，移植自 tree-sitter-c-sharp 的 `src/scanner.c`
//! （MIT，Copyright (c) 2014-2023 Max Brunsfeld, Damien Guard, Amaan Qureshi, and contributors）。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_alpha, is_space};

const OPT_SEMI: usize = 0;
const INTERPOLATION_REGULAR_START: usize = 1;
const INTERPOLATION_VERBATIM_START: usize = 2;
const INTERPOLATION_RAW_START: usize = 3;
const INTERPOLATION_START_QUOTE: usize = 4;
const INTERPOLATION_END_QUOTE: usize = 5;
const INTERPOLATION_OPEN_BRACE: usize = 6;
const INTERPOLATION_CLOSE_BRACE: usize = 7;
const INTERPOLATION_STRING_CONTENT: usize = 8;
const RAW_STRING_START: usize = 9;
const RAW_STRING_END: usize = 10;
const RAW_STRING_CONTENT: usize = 11;
const LAMBDA_PAREN_OPEN: usize = 12;

const REGULAR: u8 = 1;
const VERBATIM: u8 = 2;
const RAW: u8 = 4;

#[derive(Clone, Copy, Default)]
struct Interp {
    dollar_count: u8,
    open_brace_count: u8,
    quote_count: u8,
    string_type: u8,
}

#[derive(Default)]
pub struct CSharp {
    quote_count: u8,
    stack: Vec<Interp>,
}

fn is_id_start(c: char) -> bool {
    c == '_' || is_alpha(c)
}
fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

fn skip_ws_and_comments(lx: &mut ScanLexer) {
    loop {
        let c = lx.lookahead();
        if matches!(c, ' ' | '\t' | '\r' | '\n') {
            lx.next();
        } else if c == '/' {
            lx.next();
            if lx.lookahead() == '/' {
                while lx.lookahead() != '\0' && lx.lookahead() != '\n' {
                    lx.next();
                }
            } else if lx.lookahead() == '*' {
                lx.next();
                let mut prev = '\0';
                while lx.lookahead() != '\0' && !(prev == '*' && lx.lookahead() == '/') {
                    prev = lx.lookahead();
                    lx.next();
                }
                if lx.lookahead() == '/' {
                    lx.next();
                }
            } else {
                return;
            }
        } else {
            return;
        }
    }
}

enum Lambda {
    NoParen,
    FailedAfterParen,
    Success,
}

/// C# 14 的简单 lambda 参数列表探测（带修饰符的 `(ref x, out y) =>`）。
fn lambda_paren_open(lx: &mut ScanLexer) -> Lambda {
    while is_space(lx.lookahead()) {
        lx.skip();
    }
    if lx.lookahead() != '(' {
        return Lambda::NoParen;
    }
    lx.next();
    lx.mark_end();
    let mut saw_hard = false;
    let mut expecting = true;
    loop {
        skip_ws_and_comments(lx);
        let c = lx.lookahead();
        if c == '\0' {
            return Lambda::FailedAfterParen;
        }
        if c == ')' {
            lx.next();
            skip_ws_and_comments(lx);
            if !saw_hard || lx.lookahead() != '=' {
                return Lambda::FailedAfterParen;
            }
            lx.next();
            if lx.lookahead() != '>' {
                return Lambda::FailedAfterParen;
            }
            lx.set_result(LAMBDA_PAREN_OPEN as u16);
            return Lambda::Success;
        }
        if !expecting {
            if c != ',' {
                return Lambda::FailedAfterParen;
            }
            lx.next();
            expecting = true;
            continue;
        }
        let mut consumed_name = false;
        while !consumed_name {
            skip_ws_and_comments(lx);
            if !is_id_start(lx.lookahead()) {
                return Lambda::FailedAfterParen;
            }
            // 与官方一样把整个标识符读进 9 字节缓冲（字符截断成 8 位）
            let mut buf = [0u8; 9];
            let mut n = 0usize;
            while is_id_continue(lx.lookahead()) {
                if n < buf.len() {
                    buf[n] = (lx.lookahead() as u32 & 0xFF) as u8;
                }
                n += 1;
                lx.next();
            }
            let word = &buf[..n.min(9)];
            let hard = n <= 8 && matches!(word, b"ref" | b"out" | b"in" | b"readonly");
            let soft = n == 6 && word == b"scoped";
            if hard {
                saw_hard = true;
            } else if !soft {
                consumed_name = true;
            }
        }
        expecting = false;
    }
}

impl Scanner for CSharp {
    fn serialize(&self, out: &mut Vec<u8>) {
        if self.stack.len() * 4 + 2 > MAX_SERIALIZED {
            return;
        }
        out.push(self.quote_count);
        out.push(self.stack.len() as u8);
        for i in &self.stack {
            out.extend_from_slice(&[
                i.dollar_count,
                i.open_brace_count,
                i.quote_count,
                i.string_type,
            ]);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.quote_count = 0;
        self.stack.clear();
        if d.len() >= 2 {
            self.quote_count = d[0];
            let n = d[1] as usize;
            for k in 0..n {
                let p = 2 + k * 4;
                if p + 4 > d.len() {
                    break;
                }
                self.stack.push(Interp {
                    dollar_count: d[p],
                    open_brace_count: d[p + 1],
                    quote_count: d[p + 2],
                    string_type: d[p + 3],
                });
            }
        }
    }

    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool {
        let mut brace_advanced: u8 = 0;
        let mut quote_count: u8 = 0;
        let mut did_advance = false;

        if valid[LAMBDA_PAREN_OPEN] {
            match lambda_paren_open(lx) {
                Lambda::Success => return true,
                Lambda::FailedAfterParen => return false,
                Lambda::NoParen => {}
            }
        }
        if valid[OPT_SEMI] && valid[INTERPOLATION_REGULAR_START] {
            return false;
        }
        if valid[OPT_SEMI] {
            lx.set_result(OPT_SEMI as u16);
            if lx.lookahead() == ';' {
                lx.next();
            }
            return true;
        }
        if valid[RAW_STRING_START] {
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            if lx.lookahead() == '"' {
                while lx.lookahead() == '"' {
                    lx.next();
                    quote_count = quote_count.wrapping_add(1);
                }
                if quote_count >= 3 {
                    lx.set_result(RAW_STRING_START as u16);
                    self.quote_count = quote_count;
                    return true;
                }
            }
        }
        if valid[RAW_STRING_END] && lx.lookahead() == '"' {
            while lx.lookahead() == '"' {
                lx.next();
                quote_count = quote_count.wrapping_add(1);
            }
            if quote_count == self.quote_count {
                lx.set_result(RAW_STRING_END as u16);
                self.quote_count = 0;
                return true;
            }
            did_advance = quote_count > 0;
        }
        if valid[RAW_STRING_CONTENT] {
            while lx.lookahead() != '\0' {
                if lx.lookahead() == '"' {
                    lx.mark_end();
                    quote_count = 0;
                    while lx.lookahead() == '"' {
                        lx.next();
                        quote_count = quote_count.wrapping_add(1);
                    }
                    if quote_count == self.quote_count {
                        lx.set_result(RAW_STRING_CONTENT as u16);
                        return true;
                    }
                }
                lx.next();
            }
            lx.mark_end();
            lx.set_result(RAW_STRING_CONTENT as u16);
            return true;
        }
        if valid[INTERPOLATION_REGULAR_START]
            || valid[INTERPOLATION_VERBATIM_START]
            || valid[INTERPOLATION_RAW_START]
        {
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            let mut dollars: u8 = 0;
            let mut verbatim = false;
            if lx.lookahead() == '@' {
                verbatim = true;
                lx.next();
            }
            while lx.lookahead() == '$' && quote_count == 0 {
                lx.next();
                dollars = dollars.wrapping_add(1);
            }
            if dollars > 0 && (lx.lookahead() == '"' || lx.lookahead() == '@') {
                lx.set_result(INTERPOLATION_REGULAR_START as u16);
                let mut it = Interp {
                    dollar_count: dollars,
                    ..Default::default()
                };
                if verbatim || lx.lookahead() == '@' {
                    if lx.lookahead() == '@' {
                        lx.next();
                        verbatim = true;
                    }
                    lx.set_result(INTERPOLATION_VERBATIM_START as u16);
                    it.string_type = VERBATIM;
                }
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '"' && !verbatim {
                    lx.next();
                    if lx.lookahead() == '"' {
                        lx.set_result(INTERPOLATION_RAW_START as u16);
                        it.string_type |= RAW;
                        self.stack.push(it);
                    }
                } else {
                    it.string_type |= REGULAR;
                    self.stack.push(it);
                }
                return true;
            }
        }
        if valid[INTERPOLATION_START_QUOTE] && !self.stack.is_empty() {
            let cur = self.stack.last_mut().unwrap();
            if cur.string_type & (VERBATIM | REGULAR) != 0 {
                if lx.lookahead() == '"' {
                    lx.next();
                    cur.quote_count = cur.quote_count.wrapping_add(1);
                }
            } else {
                while lx.lookahead() == '"' {
                    lx.next();
                    cur.quote_count = cur.quote_count.wrapping_add(1);
                }
            }
            lx.set_result(INTERPOLATION_START_QUOTE as u16);
            return cur.quote_count > 0;
        }
        if valid[INTERPOLATION_END_QUOTE] && !self.stack.is_empty() {
            let cur_q = self.stack.last().unwrap().quote_count;
            while lx.lookahead() == '"' {
                lx.next();
                quote_count = quote_count.wrapping_add(1);
            }
            if quote_count == cur_q {
                lx.set_result(INTERPOLATION_END_QUOTE as u16);
                self.stack.pop();
                return true;
            }
            did_advance = quote_count > 0;
        }
        if valid[INTERPOLATION_OPEN_BRACE] && !self.stack.is_empty() {
            let cur = self.stack.last_mut().unwrap();
            while lx.lookahead() == '{' && brace_advanced < cur.dollar_count {
                lx.next();
                brace_advanced += 1;
            }
            if brace_advanced > 0 && brace_advanced == cur.dollar_count && lx.lookahead() != '{' {
                cur.open_brace_count = brace_advanced;
                lx.set_result(INTERPOLATION_OPEN_BRACE as u16);
                return true;
            }
        }
        if valid[INTERPOLATION_CLOSE_BRACE] && !self.stack.is_empty() {
            let mut braces: u8 = 0;
            let cur = self.stack.last_mut().unwrap();
            while is_space(lx.lookahead()) {
                lx.next();
            }
            while lx.lookahead() == '}' {
                lx.next();
                braces = braces.wrapping_add(1);
                if braces == cur.open_brace_count {
                    cur.open_brace_count = 0;
                    lx.set_result(INTERPOLATION_CLOSE_BRACE as u16);
                    return true;
                }
            }
            return false;
        }
        if valid[INTERPOLATION_STRING_CONTENT] && !self.stack.is_empty() {
            lx.set_result(INTERPOLATION_STRING_CONTENT as u16);
            let cur = *self.stack.last().unwrap();
            while lx.lookahead() != '\0' {
                if cur.string_type & RAW != 0 {
                    if lx.lookahead() == '"' {
                        lx.mark_end();
                        lx.next();
                        if lx.lookahead() == '"' {
                            lx.next();
                            let mut q: u8 = 2;
                            while lx.lookahead() == '"' {
                                q = q.wrapping_add(1);
                                lx.next();
                            }
                            if q == cur.quote_count {
                                return did_advance;
                            }
                        }
                    }
                    if lx.lookahead() == '{' {
                        lx.mark_end();
                        while lx.lookahead() == '{' && brace_advanced < cur.open_brace_count {
                            lx.next();
                            brace_advanced += 1;
                        }
                        if brace_advanced == cur.open_brace_count
                            && (brace_advanced == 0 || lx.lookahead() != '{')
                        {
                            return did_advance;
                        }
                    }
                } else if cur.string_type & VERBATIM != 0 {
                    if lx.lookahead() == '"' {
                        lx.mark_end();
                        lx.next();
                        if lx.lookahead() == '"' {
                            lx.next();
                            continue;
                        }
                        return did_advance;
                    }
                    if lx.lookahead() == '{' {
                        lx.mark_end();
                        while lx.lookahead() == '{' && brace_advanced < cur.open_brace_count {
                            lx.next();
                            brace_advanced += 1;
                        }
                        if brace_advanced == cur.open_brace_count
                            && (brace_advanced == 0 || lx.lookahead() != '{')
                        {
                            return did_advance;
                        }
                    }
                } else if cur.string_type & REGULAR != 0 {
                    if matches!(lx.lookahead(), '\\' | '\n' | '"') {
                        lx.mark_end();
                        return did_advance;
                    }
                    if lx.lookahead() == '{' {
                        lx.mark_end();
                        while lx.lookahead() == '{' && brace_advanced < cur.open_brace_count {
                            lx.next();
                            brace_advanced += 1;
                        }
                        if brace_advanced == cur.open_brace_count
                            && (brace_advanced == 0 || lx.lookahead() != '{')
                        {
                            return did_advance;
                        }
                    }
                }
                if lx.lookahead() != '{' {
                    brace_advanced = 0;
                }
                lx.next();
                did_advance = true;
            }
            lx.mark_end();
            return did_advance;
        }
        false
    }
}
