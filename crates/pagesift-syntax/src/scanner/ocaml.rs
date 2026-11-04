//! OCaml 外部扫描器（`ocaml`、`interface`、`type` 三个语法共用），移植自 tree-sitter-ocaml 的
//! `common/scanner.h`（MIT，Copyright (c) 2020 Max Brunsfeld and Pieter Goetschalckx）。
//!
//! 官方序列化把引用字符串的分隔符数组按“元素个数”当字节数拷贝（只拷前几个字节），反序列化时再按字节数
//! 当元素个数还原；这里照原样保留，多出来的元素补 0。

use super::{ScanLexer, Scanner, is_space};

const COMMENT: usize = 0;
const LEFT_QUOTED_STRING_DELIM: usize = 1;
const RIGHT_QUOTED_STRING_DELIM: usize = 2;
const STRING_DELIM: usize = 3;
const LINE_NUMBER_DIRECTIVE: usize = 4;
const NULL_CHARACTER: usize = 5;
const ERROR_SENTINEL: usize = 6;

const LOWER_UTF8_CHARS: [u32; 35] = [
    0xdf, 0xe0, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xeb, 0xec, 0xed, 0xee,
    0xef, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff,
    0x153, 0x161, 0x17e,
];

const LOWER_UTF8_PAIRS: [(u32, u32, u32); 29] = [
    ('a' as u32, 0x300, 0xe0),
    ('a' as u32, 0x301, 0xe1),
    ('a' as u32, 0x302, 0xe2),
    ('a' as u32, 0x303, 0xe3),
    ('a' as u32, 0x308, 0xe4),
    ('a' as u32, 0x30a, 0xe5),
    ('c' as u32, 0x327, 0xe7),
    ('e' as u32, 0x300, 0xe8),
    ('e' as u32, 0x301, 0xe9),
    ('e' as u32, 0x302, 0xea),
    ('e' as u32, 0x308, 0xeb),
    ('i' as u32, 0x300, 0xec),
    ('i' as u32, 0x301, 0xed),
    ('i' as u32, 0x302, 0xee),
    ('i' as u32, 0x308, 0xef),
    ('n' as u32, 0x303, 0xf1),
    ('o' as u32, 0x300, 0xf2),
    ('o' as u32, 0x301, 0xf3),
    ('o' as u32, 0x302, 0xf4),
    ('o' as u32, 0x303, 0xf5),
    ('o' as u32, 0x308, 0xf6),
    ('s' as u32, 0x30c, 0x161),
    ('u' as u32, 0x300, 0xf9),
    ('u' as u32, 0x301, 0xfa),
    ('u' as u32, 0x302, 0xfb),
    ('u' as u32, 0x308, 0xfc),
    ('y' as u32, 0x301, 0xfd),
    ('y' as u32, 0x308, 0xff),
    ('z' as u32, 0x30c, 0x17e),
];

#[derive(Default)]
pub struct Ocaml {
    in_string: bool,
    /// 分隔符数组的底层内存：清空只改 `len`，内容保留（官方如此，反序列化的“半截覆盖”靠它复原）。
    mem: Vec<u32>,
    len: usize,
}

impl Ocaml {
    fn grow(&mut self, n: usize) {
        if self.mem.len() < n {
            self.mem.resize(n, 0);
        }
    }
}

fn la(lx: &ScanLexer) -> u32 {
    lx.lookahead() as u32
}

fn digit(lx: &ScanLexer) -> bool {
    lx.lookahead().is_ascii_digit()
}

fn scan_string(lx: &mut ScanLexer) {
    loop {
        match lx.lookahead() {
            '\\' => {
                lx.next();
                lx.next();
            }
            '"' => {
                lx.next();
                return;
            }
            '\0' => {
                if lx.eof() {
                    return;
                }
                lx.next();
            }
            _ => lx.next(),
        }
    }
}

/// 引用字符串分隔符里的一个字符；遇到 `|` 或不合法字符返回 0。
fn scan_delim_char(lx: &mut ScanLexer) -> u32 {
    let c = la(lx);
    if c == '|' as u32 {
        return 0;
    }
    if c == '_' as u32 {
        lx.next();
        return c;
    }
    if (b'a' as u32..=b'z' as u32).contains(&c) {
        lx.next();
        let d = la(lx);
        if (0x300..=0x327).contains(&d)
            && let Some(&(_, _, r)) = LOWER_UTF8_PAIRS.iter().find(|p| p.0 == c && p.1 == d)
        {
            lx.next();
            return r;
        }
        return c;
    }
    if LOWER_UTF8_CHARS.contains(&c) {
        lx.next();
        return c;
    }
    0
}

fn is_lowercase_ext(c: u32) -> bool {
    (b'a' as u32..=b'z' as u32).contains(&c) || c == '_' as u32 || c >= 192
}

fn is_identstart(c: u32) -> bool {
    is_lowercase_ext(c) || (b'A' as u32..=b'Z' as u32).contains(&c)
}

fn is_identchar(c: u32) -> bool {
    is_identstart(c) || (b'0' as u32..=b'9' as u32).contains(&c) || c == '\'' as u32
}

fn scan_identifier(lx: &mut ScanLexer) -> bool {
    if is_identstart(la(lx)) {
        lx.next();
        while is_identchar(la(lx)) {
            lx.next();
        }
        return true;
    }
    false
}

fn scan_extattrident(lx: &mut ScanLexer) -> bool {
    while scan_identifier(lx) {
        if lx.lookahead() != '.' {
            return true;
        }
        lx.next();
    }
    false
}

/// 字符字面量：返回最后一个字符（0 表示不是字符字面量或没有可回退的字符）。
fn scan_character(lx: &mut ScanLexer) -> u32 {
    let mut last = 0u32;
    match lx.lookahead() {
        '\\' => {
            lx.next();
            if digit(lx) {
                lx.next();
                for _ in 0..2 {
                    if !digit(lx) {
                        return 0;
                    }
                    lx.next();
                }
            } else {
                match lx.lookahead() {
                    'x' => {
                        lx.next();
                        for _ in 0..2 {
                            if !digit(lx)
                                && !lx.lookahead().to_ascii_uppercase().is_ascii_hexdigit()
                            {
                                return 0;
                            }
                            lx.next();
                        }
                    }
                    'o' => {
                        lx.next();
                        for _ in 0..3 {
                            if !digit(lx) || lx.lookahead() > '7' {
                                return 0;
                            }
                            lx.next();
                        }
                    }
                    '\'' | '"' | '\\' | 'n' | 't' | 'b' | 'r' | ' ' => {
                        last = la(lx);
                        lx.next();
                    }
                    _ => return 0,
                }
            }
        }
        '\'' => {}
        '\r' => {
            lx.next();
            while lx.lookahead() == '\r' {
                lx.next();
            }
            if lx.lookahead() != '\n' {
                return 0;
            }
            lx.next();
        }
        '\0' => {
            if lx.eof() {
                return 0;
            }
            lx.next();
        }
        _ => {
            if la(lx) < 256 {
                last = la(lx);
                lx.next();
            } else {
                return 0;
            }
        }
    }
    if lx.lookahead() == '\'' {
        lx.next();
        return 0;
    }
    last
}

impl Ocaml {
    fn scan_left_delim(&mut self, lx: &mut ScanLexer) -> bool {
        loop {
            let c = scan_delim_char(lx);
            if c == 0 {
                break;
            }
            self.grow(self.len + 1);
            self.mem[self.len] = c;
            self.len += 1;
        }
        if lx.lookahead() == '|' {
            lx.next();
            self.in_string = true;
            true
        } else {
            self.len = 0;
            false
        }
    }

    fn scan_right_delim(&mut self, lx: &mut ScanLexer) -> bool {
        for i in 0..self.len {
            if scan_delim_char(lx) != self.mem[i] {
                return false;
            }
        }
        if lx.lookahead() == '}' {
            self.in_string = false;
            self.len = 0;
            true
        } else {
            false
        }
    }

    fn scan_quoted_string(&mut self, lx: &mut ScanLexer) -> bool {
        if !self.scan_left_delim(lx) {
            return false;
        }
        loop {
            match lx.lookahead() {
                '|' => {
                    lx.next();
                    if self.scan_right_delim(lx) {
                        return true;
                    }
                }
                '\0' => {
                    if lx.eof() {
                        return false;
                    }
                    lx.next();
                }
                _ => lx.next(),
            }
        }
    }

    fn scan_comment(&mut self, lx: &mut ScanLexer) -> bool {
        let mut last = 0u32;
        let mut depth = 0u32;
        if lx.lookahead() != '*' {
            return false;
        }
        lx.next();
        // 官方 `last` 表示“上一个字符已读出但没前进”；非 0 时先消耗它
        let eat = |lx: &mut ScanLexer, last: &mut u32| {
            if *last != 0 {
                *last = 0;
            } else {
                lx.next();
            }
        };
        loop {
            let cur = if last != 0 { last } else { la(lx) };
            match char::from_u32(cur).unwrap_or('\u{FFFD}') {
                '(' => {
                    eat(lx, &mut last);
                    if lx.lookahead() == '*' {
                        lx.next();
                        depth += 1;
                    }
                }
                '*' => {
                    eat(lx, &mut last);
                    if lx.lookahead() == ')' {
                        lx.next();
                        if depth == 0 {
                            return true;
                        }
                        depth -= 1;
                    }
                }
                '\'' => {
                    eat(lx, &mut last);
                    last = scan_character(lx);
                }
                '"' => {
                    eat(lx, &mut last);
                    scan_string(lx);
                }
                '{' => {
                    eat(lx, &mut last);
                    if lx.lookahead() == '%' {
                        lx.next();
                        if lx.lookahead() == '%' {
                            lx.next();
                        }
                        if scan_extattrident(lx) {
                            while is_space(lx.lookahead()) {
                                lx.next();
                            }
                        } else {
                            continue;
                        }
                    }
                    if self.scan_quoted_string(lx) {
                        lx.next();
                    }
                }
                '\0' => {
                    if lx.eof() {
                        return true;
                    }
                    eat(lx, &mut last);
                }
                _ => {
                    if scan_identifier(lx) || last != 0 {
                        last = 0;
                    } else {
                        lx.next();
                    }
                }
            }
        }
    }
}

impl Scanner for Ocaml {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if !v[ERROR_SENTINEL]
            && v[LEFT_QUOTED_STRING_DELIM]
            && (is_lowercase_ext(la(lx)) || lx.lookahead() == '|')
        {
            lx.set_result(LEFT_QUOTED_STRING_DELIM as u16);
            return self.scan_left_delim(lx);
        }
        if !v[ERROR_SENTINEL] && v[RIGHT_QUOTED_STRING_DELIM] && lx.lookahead() == '|' {
            lx.next();
            lx.set_result(RIGHT_QUOTED_STRING_DELIM as u16);
            return self.scan_right_delim(lx);
        }
        if self.in_string && v[STRING_DELIM] && lx.lookahead() == '"' {
            lx.next();
            self.in_string = false;
            lx.set_result(STRING_DELIM as u16);
            return true;
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if !self.in_string && v[STRING_DELIM] && lx.lookahead() == '"' {
            lx.next();
            self.in_string = true;
            lx.set_result(STRING_DELIM as u16);
            return true;
        }
        if !self.in_string && v[LINE_NUMBER_DIRECTIVE] && lx.lookahead() == '#' && lx.column() == 0
        {
            lx.next();
            while matches!(lx.lookahead(), ' ' | '\t') {
                lx.next();
            }
            if !digit(lx) {
                return false;
            }
            while digit(lx) {
                lx.next();
            }
            while matches!(lx.lookahead(), ' ' | '\t') {
                lx.next();
            }
            if lx.lookahead() != '"' {
                return false;
            }
            lx.next();
            while !matches!(lx.lookahead(), '\n' | '\r' | '"') && !lx.eof() {
                lx.next();
            }
            if lx.lookahead() != '"' {
                return false;
            }
            lx.next();
            while !matches!(lx.lookahead(), '\n' | '\r') && !lx.eof() {
                lx.next();
            }
            lx.set_result(LINE_NUMBER_DIRECTIVE as u16);
            return true;
        }
        if !self.in_string && v[COMMENT] && lx.lookahead() == '(' {
            lx.next();
            lx.set_result(COMMENT as u16);
            return self.scan_comment(lx);
        }
        if v[NULL_CHARACTER] && lx.lookahead() == '\0' && !lx.eof() {
            lx.next();
            lx.set_result(NULL_CHARACTER as u16);
            return true;
        }
        false
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.in_string as u8);
        let n = self.len;
        if n < super::MAX_SERIALIZED {
            let bytes: Vec<u8> = self.mem.iter().flat_map(|c| c.to_le_bytes()).collect();
            out.extend_from_slice(&bytes[..n.min(bytes.len())]);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        if d.is_empty() {
            self.in_string = false;
            self.len = 0;
            return;
        }
        self.in_string = d[0] != 0;
        let n = d.len() - 1;
        if n > 0 {
            self.grow(n);
            for (i, b) in d[1..].iter().enumerate() {
                let sh = 8 * (i % 4);
                self.mem[i / 4] = (self.mem[i / 4] & !(0xff << sh)) | ((*b as u32) << sh);
            }
        }
        self.len = n;
    }
}
