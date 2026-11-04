//! D 外部扫描器，移植自 tree-sitter-d 的 `src/scanner.c`（MIT，Copyright 2024 Garrett D'Amore）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner, is_space};

const DIRECTIVE: usize = 0;
const L_INT: usize = 1;
const L_FLOAT: usize = 2;
const L_STRING: usize = 3;
const NOT_IN: usize = 4;
const NOT_IS: usize = 5;
const AFTER_EOF: usize = 6;
const ERROR: usize = 7;

const HEREDOC_DELIM_MAX: usize = 256;

#[derive(Default)]
pub struct D;

fn is_eol(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn alnum(c: char) -> bool {
    c.is_alphanumeric()
}

/// 字符串可选的 `c`/`d`/`w` 后缀；总是成功，要么延长匹配要么不延长。
fn match_string_suffix(lx: &mut ScanLexer) {
    if matches!(lx.lookahead(), 'c' | 'd' | 'w') {
        lx.next();
    }
    lx.mark_end();
}

fn match_delimited_string(lx: &mut ScanLexer, start: char, end: char) -> bool {
    let mut nest = 0u32;
    let mut first = true;
    lx.next();
    while lx.lookahead() != '\0' {
        let c = lx.lookahead();
        if c == start && start != '\0' {
            nest += 1;
        }
        if c == end {
            if nest > 0 {
                nest -= 1;
            } else if !first {
                lx.next();
                if lx.lookahead() != '"' {
                    // 已经前进过了，不能再前进（保证 `}}"` 这类情形可用）
                    continue;
                }
                lx.next();
                lx.set_result(L_STRING as u16);
                match_string_suffix(lx);
                return true;
            }
        }
        first = false;
        lx.next();
    }
    false
}

fn match_heredoc_string(lx: &mut ScanLexer) -> bool {
    let mut id: Vec<char> = Vec::new();
    while id.len() < HEREDOC_DELIM_MAX {
        let c = lx.lookahead();
        if is_eol(c) || (!alnum(c) && c != '_') {
            break;
        }
        id.push(c);
        lx.next();
    }
    if id.is_empty() {
        return false;
    }
    // 填满了缓冲区但分隔符还在继续：太长，放弃
    let c = lx.lookahead();
    if id.len() == HEREDOC_DELIM_MAX && !is_eol(c) && (alnum(c) || c == '_') {
        return false;
    }
    id.push('"');
    while lx.lookahead() != '\0' {
        while !is_eol(lx.lookahead()) && lx.lookahead() != '\0' {
            lx.next();
        }
        lx.next();
        let mut j = 0;
        while lx.lookahead() != '\0' && j < id.len() {
            if lx.lookahead() != id[j] {
                break;
            }
            lx.next();
            j += 1;
        }
        if j == id.len() {
            match_string_suffix(lx);
            lx.set_result(L_STRING as u16);
            return true;
        }
    }
    false
}

fn match_directive(lx: &mut ScanLexer, v: &[bool]) -> bool {
    if !v[DIRECTIVE] {
        return false;
    }
    lx.next();
    if lx.lookahead() == '!' {
        return false;
    }
    while (is_space(lx.lookahead()) || is_eol(lx.lookahead())) && lx.lookahead() != '\0' {
        if is_eol(lx.lookahead()) {
            return false;
        }
        lx.next();
    }
    while !is_eol(lx.lookahead()) && lx.lookahead() != '\0' {
        lx.next();
    }
    lx.next();
    lx.mark_end();
    lx.set_result(DIRECTIVE as u16);
    true
}

fn match_number_suffix(lx: &mut ScanLexer, v: &[bool], is_float: bool) -> bool {
    let (mut seen_l, mut seen_i, mut seen_u, mut seen_f) = (false, false, false, false);
    let mut tok = 0usize;
    loop {
        let c = lx.lookahead();
        if c == '\0' {
            break;
        }
        match c {
            'u' | 'U' => {
                if seen_u || seen_i || seen_f || is_float {
                    return false;
                }
                seen_u = true;
                tok = L_INT;
            }
            'f' | 'F' => {
                if seen_u || seen_f || seen_i {
                    return false;
                }
                seen_f = true;
                tok = L_FLOAT;
            }
            'i' => {
                // 虚数浮点，必须是最后一个
                if seen_i || seen_u {
                    return false;
                }
                tok = L_FLOAT;
                seen_i = true;
            }
            'L' => {
                if seen_l || seen_f || seen_i {
                    return false;
                }
                seen_l = true;
            }
            _ => break,
        }
        lx.next();
    }
    // 后缀后面不能再跟数字或字母
    let c = lx.lookahead();
    if alnum(c) || (c as u32 > 0x7f && !is_eol(c)) {
        return false;
    }
    if is_float {
        tok = L_FLOAT;
    }
    if v[L_INT] && tok != L_FLOAT {
        lx.set_result(L_INT as u16);
        lx.mark_end();
        return true;
    }
    if v[L_FLOAT] && tok != L_INT {
        lx.set_result(L_FLOAT as u16);
        lx.mark_end();
        return true;
    }
    false
}

fn match_number(lx: &mut ScanLexer, v: &[bool]) -> bool {
    let mut c = lx.lookahead();
    let (mut is_hex, mut is_bin, mut has_digit, mut has_dot, mut in_exp) =
        (false, false, false, false, false);
    if c == '.' {
        lx.next();
        if !lx.lookahead().is_ascii_digit() {
            return false;
        }
        has_dot = true;
    } else if c == '0' {
        lx.next();
        match lx.lookahead() {
            'b' | 'B' => {
                is_bin = true;
                lx.next();
            }
            'x' | 'X' => {
                is_hex = true;
                lx.next();
            }
            _ => has_digit = true,
        }
    }
    if !(v[L_INT] || v[L_FLOAT]) {
        return false;
    }
    let mut done = false;
    while lx.lookahead() != '\0' && !done {
        c = lx.lookahead();
        if c as u32 > 0x7f || is_space(c) || c == ';' {
            break;
        }
        if (is_bin && matches!(c, '0' | '1'))
            || c.is_ascii_digit()
            || (is_hex && !in_exp && c.is_ascii_hexdigit())
        {
            lx.next();
            lx.mark_end();
            has_digit = true;
            continue;
        }
        match c {
            '.' => {
                if !has_digit || has_dot || in_exp || is_bin {
                    lx.mark_end();
                    done = true;
                    continue;
                }
                lx.mark_end();
                lx.next();
                c = lx.lookahead();
                if c.is_ascii_digit() || (is_hex && c.is_ascii_hexdigit()) {
                    has_dot = true;
                    continue;
                }
                // 后面像属性访问就只取整数；否则把点吃掉当浮点数（和 DMD 一样）
                if alnum(c) || c == '_' || c == '.' || (c as u32 > 0x7f && !is_eol(c)) {
                    lx.set_result(L_INT as u16);
                    return v[L_INT];
                }
                lx.set_result(L_FLOAT as u16);
                lx.mark_end();
                return v[L_FLOAT];
            }
            '_' => lx.next(),
            'e' | 'E' | 'p' | 'P' => {
                if in_exp || is_bin {
                    return false;
                }
                if is_hex && matches!(c, 'e' | 'E') {
                    return false;
                }
                if !is_hex && matches!(c, 'p' | 'P') {
                    return false;
                }
                lx.next();
                if matches!(lx.lookahead(), '+' | '-') {
                    lx.next();
                }
                has_digit = false;
                in_exp = true;
            }
            _ => done = true,
        }
    }
    if !has_digit {
        return false;
    }
    match_number_suffix(lx, v, has_dot || in_exp)
}

fn match_not_in_is(lx: &mut ScanLexer, v: &[bool]) -> bool {
    if !v[NOT_IN] && !v[NOT_IS] {
        return false;
    }
    lx.next();
    while lx.lookahead() != '\0' && (is_space(lx.lookahead()) || is_eol(lx.lookahead())) {
        lx.next();
    }
    if lx.lookahead() != 'i' {
        return false;
    }
    lx.next();
    let token = match lx.lookahead() {
        'n' => NOT_IN,
        's' => NOT_IS,
        _ => return false,
    };
    if !v[token] {
        return false;
    }
    lx.next();
    let c = lx.lookahead();
    if alnum(c) || (c as u32 > 0x7f && !is_eol(c)) {
        return false;
    }
    lx.set_result(token as u16);
    lx.mark_end();
    true
}

impl Scanner for D {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let mut c = lx.lookahead();
        let mut start_of_line = lx.column() == 0;
        if v[AFTER_EOF] && !v[ERROR] {
            while lx.lookahead() != '\0' {
                lx.skip();
            }
            lx.mark_end();
            lx.set_result(AFTER_EOF as u16);
            return true;
        }
        while (is_space(c) || is_eol(c)) && c != '\0' {
            if is_eol(c) {
                start_of_line = true;
            }
            lx.skip();
            c = lx.lookahead();
        }
        if c == '#' && start_of_line {
            return match_directive(lx, v);
        }
        if lx.eof() {
            return false;
        }
        if c == '.' || c.is_ascii_digit() {
            return match_number(lx, v);
        }
        // `!in` 和 `!is` 要当成独立记号（`func!int` 是模板参数）
        if c == '!' {
            return match_not_in_is(lx, v);
        }
        if c == 'q' && v[L_STRING] {
            lx.next();
            if lx.lookahead() != '"' {
                return false;
            }
            lx.next();
            let c = lx.lookahead();
            return match c {
                '(' => match_delimited_string(lx, '(', ')'),
                '[' => match_delimited_string(lx, '[', ']'),
                '{' => match_delimited_string(lx, '{', '}'),
                '<' => match_delimited_string(lx, '<', '>'),
                _ => {
                    if alnum(c) || c == '_' {
                        match_heredoc_string(lx)
                    } else {
                        match_delimited_string(lx, '\0', c)
                    }
                }
            };
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
