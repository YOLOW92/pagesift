//! Ruby 外部扫描器，移植自 tree-sitter-ruby 的 `src/scanner.c`（MIT，Copyright (c) 2016 Rob Rix）。
//! 序列化格式与官方一致；`is_at_included_range_start` 在本运行时恒为假，故省略。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_alpha, is_space};

const LINE_BREAK: usize = 0;
const NO_LINE_BREAK: usize = 1;
const SIMPLE_SYMBOL: usize = 2;
const STRING_START: usize = 3;
const SYMBOL_START: usize = 4;
const SUBSHELL_START: usize = 5;
const REGEX_START: usize = 6;
const STRING_ARRAY_START: usize = 7;
const SYMBOL_ARRAY_START: usize = 8;
const HEREDOC_BODY_START: usize = 9;
const STRING_CONTENT: usize = 10;
const HEREDOC_CONTENT: usize = 11;
const STRING_END: usize = 12;
const HEREDOC_BODY_END: usize = 13;
const HEREDOC_START: usize = 14;
const FORWARD_SLASH: usize = 15;
const BLOCK_AMPERSAND: usize = 16;
const SPLAT_STAR: usize = 17;
const UNARY_MINUS: usize = 18;
const UNARY_MINUS_NUM: usize = 19;
const BINARY_MINUS: usize = 20;
const BINARY_STAR: usize = 21;
const SINGLETON_CLASS_LEFT_ANGLE_LEFT_ANGLE: usize = 22;
const HASH_KEY_SYMBOL: usize = 23;
const IDENTIFIER_SUFFIX: usize = 24;
const CONSTANT_SUFFIX: usize = 25;
const HASH_SPLAT_STAR_STAR: usize = 26;
const BINARY_STAR_STAR: usize = 27;
const ELEMENT_REFERENCE_BRACKET: usize = 28;
const SHORT_INTERPOLATION: usize = 29;
const NONE: u16 = 30;

const NON_IDENTIFIER_CHARS: &[u8] = b"\0\n\r\t :;`\"'@$#.,|^&<=>+-*/\\%?!~()[]{}";

#[derive(Clone, Default)]
struct Literal {
    kind: u8,
    open: i32,
    close: i32,
    depth: i32,
    interp: bool,
}

#[derive(Clone, Default)]
struct Heredoc {
    /// 官方用 `char` 数组，非 ASCII 字节按有符号数比较。
    word: Vec<u8>,
    indent_ok: bool,
    interp: bool,
    started: bool,
}

#[derive(Default)]
pub struct Ruby {
    leading_ws: bool,
    literals: Vec<Literal>,
    heredocs: Vec<Heredoc>,
}

fn alnum(c: char) -> bool {
    c.is_alphanumeric()
}

/// 官方把前瞻转成 `char` 再查表，所以只看低 8 位。
fn is_iden_char(c: char) -> bool {
    !NON_IDENTIFIER_CHARS.contains(&(c as u32 as u8))
}

fn scan_operator(lx: &mut ScanLexer) -> bool {
    match lx.lookahead() {
        '<' => {
            lx.next();
            if lx.lookahead() == '<' {
                lx.next();
            } else if lx.lookahead() == '=' {
                lx.next();
                if lx.lookahead() == '>' {
                    lx.next();
                }
            }
            true
        }
        '>' => {
            lx.next();
            if matches!(lx.lookahead(), '>' | '=') {
                lx.next();
            }
            true
        }
        '=' => {
            lx.next();
            if lx.lookahead() == '~' {
                lx.next();
                return true;
            }
            if lx.lookahead() == '=' {
                lx.next();
                if lx.lookahead() == '=' {
                    lx.next();
                }
                return true;
            }
            false
        }
        '+' | '-' | '~' => {
            lx.next();
            if lx.lookahead() == '@' {
                lx.next();
            }
            true
        }
        '.' => {
            lx.next();
            if lx.lookahead() == '.' {
                lx.next();
                return true;
            }
            false
        }
        '&' | '^' | '|' | '/' | '%' | '`' => {
            lx.next();
            true
        }
        '!' => {
            lx.next();
            if matches!(lx.lookahead(), '=' | '~') {
                lx.next();
            }
            true
        }
        '*' => {
            lx.next();
            if lx.lookahead() == '*' {
                lx.next();
            }
            true
        }
        '[' => {
            lx.next();
            if lx.lookahead() == ']' {
                lx.next();
            } else {
                return false;
            }
            if lx.lookahead() == '=' {
                lx.next();
            }
            true
        }
        _ => false,
    }
}

fn scan_symbol_identifier(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() == '@' {
        lx.next();
        if lx.lookahead() == '@' {
            lx.next();
        }
    } else if lx.lookahead() == '$' {
        lx.next();
    }
    if is_iden_char(lx.lookahead()) {
        lx.next();
    } else if !scan_operator(lx) {
        return false;
    }
    while is_iden_char(lx.lookahead()) {
        lx.next();
    }
    if matches!(lx.lookahead(), '?' | '!') {
        lx.next();
    }
    if lx.lookahead() == '=' {
        lx.mark_end();
        lx.next();
        if lx.lookahead() != '>' {
            lx.mark_end();
        }
    }
    true
}

fn scan_heredoc_word(lx: &mut ScanLexer, h: &mut Heredoc) {
    let mut quote = '\0';
    let mut word = Vec::new();
    match lx.lookahead() {
        q @ ('\'' | '"' | '`') => {
            quote = q;
            lx.next();
            while lx.lookahead() != quote && !lx.eof() {
                word.push(lx.lookahead() as u32 as u8);
                lx.next();
            }
            lx.next();
        }
        c if alnum(c) || c == '_' => {
            while alnum(lx.lookahead()) || lx.lookahead() == '_' {
                word.push(lx.lookahead() as u32 as u8);
                lx.next();
            }
        }
        _ => {}
    }
    h.word = word;
    h.interp = quote != '\'';
}

fn scan_short_interpolation(lx: &mut ScanLexer, has_content: bool, content: usize) -> bool {
    let start = lx.lookahead();
    if start != '@' && start != '$' {
        return false;
    }
    if has_content {
        lx.set_result(content as u16);
        return true;
    }
    lx.mark_end();
    lx.next();
    let mut short = false;
    if start == '$' {
        // 官方用 strchr：前瞻为 0（输入结束）时会匹配到字符串结尾的 NUL
        let c = lx.lookahead();
        if c == '\0'
            || "!@&`'+~=/\\,;.<>*$?:\""
                .as_bytes()
                .contains(&(c as u32 as u8))
        {
            short = true;
        } else if c == '-' {
            lx.next();
            short = is_alpha(lx.lookahead()) || lx.lookahead() == '_';
        } else {
            short = alnum(c) || c == '_';
        }
    }
    if start == '@' {
        if lx.lookahead() == '@' {
            lx.next();
        }
        short = is_iden_char(lx.lookahead()) && !lx.lookahead().is_ascii_digit();
    }
    if short {
        lx.set_result(SHORT_INTERPOLATION as u16);
    }
    short
}

impl Ruby {
    fn skip(&mut self, lx: &mut ScanLexer) {
        self.leading_ws = true;
        lx.skip();
    }

    fn scan_whitespace(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let hd_start =
            !self.heredocs.is_empty() && !self.heredocs[0].started && v[HEREDOC_BODY_START];
        let mut crossed = false;
        loop {
            match lx.lookahead() {
                ' ' | '\t' => self.skip(lx),
                '\r' => {
                    if hd_start {
                        lx.set_result(HEREDOC_BODY_START as u16);
                        self.heredocs[0].started = true;
                        return true;
                    }
                    self.skip(lx);
                }
                '\n' => {
                    if hd_start {
                        lx.set_result(HEREDOC_BODY_START as u16);
                        self.heredocs[0].started = true;
                        return true;
                    } else if !v[NO_LINE_BREAK] && v[LINE_BREAK] && !crossed {
                        lx.mark_end();
                        lx.next();
                        crossed = true;
                    } else {
                        self.skip(lx);
                    }
                }
                '\\' => {
                    lx.next();
                    if lx.lookahead() == '\r' {
                        self.skip(lx);
                    }
                    if is_space(lx.lookahead()) {
                        self.skip(lx);
                    } else {
                        return false;
                    }
                }
                c => {
                    if crossed {
                        if !matches!(c, '.' | '&' | '#') {
                            lx.set_result(LINE_BREAK as u16);
                        } else if c == '.' {
                            // 调用运算符 `.` 前不产生换行，范围运算符 `..` 前才产生
                            lx.next();
                            if !lx.eof() && lx.lookahead() == '.' {
                                lx.set_result(LINE_BREAK as u16);
                            } else {
                                return false;
                            }
                        }
                    }
                    return true;
                }
            }
        }
    }

    fn open_delimiter(&mut self, lx: &mut ScanLexer, lit: &mut Literal, v: &[bool]) -> bool {
        let c = lx.lookahead();
        match c {
            '"' | '\'' => {
                lit.kind = STRING_START as u8;
                lit.open = c as i32;
                lit.close = c as i32;
                lit.interp = c == '"';
                lx.next();
                true
            }
            '`' => {
                if !v[SUBSHELL_START] {
                    return false;
                }
                lit.kind = SUBSHELL_START as u8;
                lit.open = c as i32;
                lit.close = c as i32;
                lit.interp = true;
                lx.next();
                true
            }
            '/' => {
                if !v[REGEX_START] {
                    return false;
                }
                lit.kind = REGEX_START as u8;
                lit.open = c as i32;
                lit.close = c as i32;
                lit.interp = true;
                lx.next();
                if v[FORWARD_SLASH]
                    && (!self.leading_ws
                        || matches!(lx.lookahead(), ' ' | '\t' | '\n' | '\r' | '='))
                {
                    return false;
                }
                true
            }
            '%' => {
                lx.next();
                // (需要的有效符号, 类型, 是否插值)；None 表示不消耗字母
                let (need, kind, interp, eat) = match lx.lookahead() {
                    's' => (SIMPLE_SYMBOL, SYMBOL_START, false, true),
                    'r' => (REGEX_START, REGEX_START, true, true),
                    'x' => (SUBSHELL_START, SUBSHELL_START, true, true),
                    'q' => (STRING_START, STRING_START, false, true),
                    'Q' => (STRING_START, STRING_START, true, true),
                    'w' => (STRING_ARRAY_START, STRING_ARRAY_START, false, true),
                    'i' => (SYMBOL_ARRAY_START, SYMBOL_ARRAY_START, false, true),
                    'W' => (STRING_ARRAY_START, STRING_ARRAY_START, true, true),
                    'I' => (SYMBOL_ARRAY_START, SYMBOL_ARRAY_START, true, true),
                    _ => (STRING_START, STRING_START, true, false),
                };
                if !v[need] {
                    return false;
                }
                lit.kind = kind as u8;
                lit.interp = interp;
                if eat {
                    lx.next();
                }
                let d = lx.lookahead();
                match d {
                    '(' | '[' | '{' | '<' => {
                        lit.open = d as i32;
                        lit.close = match d {
                            '(' => ')',
                            '[' => ']',
                            '{' => '}',
                            _ => '>',
                        } as i32;
                    }
                    '\r' | '\n' | ' ' | '\t' => {
                        // `/` 运算符有效时 `%` 也是运算符，后面跟空白就不是百分号字符串
                        if v[FORWARD_SLASH] {
                            return false;
                        }
                    }
                    '|' | '!' | '#' | '/' | '\\' | '@' | '$' | '%' | '^' | '&' | '*' | ')'
                    | ']' | '}' | '>' | '+' | '-' | '~' | '`' | ',' | '.' | '?' | ':' | ';'
                    | '_' | '"' | '\'' => {
                        lit.open = d as i32;
                        lit.close = d as i32;
                    }
                    _ => return false,
                }
                lx.next();
                true
            }
            _ => false,
        }
    }

    fn heredoc_content(&mut self, lx: &mut ScanLexer) -> bool {
        let mut pos = 0usize;
        let mut look_end = true;
        let mut has_content = false;
        loop {
            if pos == self.heredocs[0].word.len() {
                if !has_content {
                    lx.mark_end();
                }
                while matches!(lx.lookahead(), ' ' | '\t') {
                    lx.next();
                }
                if matches!(lx.lookahead(), '\n' | '\r') {
                    if has_content {
                        lx.set_result(HEREDOC_CONTENT as u16);
                    } else {
                        self.heredocs.remove(0);
                        lx.set_result(HEREDOC_BODY_END as u16);
                    }
                    return true;
                }
                has_content = true;
                pos = 0;
            }
            if lx.eof() {
                lx.mark_end();
                if has_content {
                    lx.set_result(HEREDOC_CONTENT as u16);
                } else {
                    self.heredocs.remove(0);
                    lx.set_result(HEREDOC_BODY_END as u16);
                }
                return true;
            }
            let c = lx.lookahead();
            let h = &self.heredocs[0];
            if look_end && c as i32 == h.word[pos] as i8 as i32 {
                lx.next();
                pos += 1;
                continue;
            }
            pos = 0;
            look_end = false;
            let (interp, indent_ok) = (h.interp, h.indent_ok);
            if interp && c == '\\' {
                if has_content {
                    lx.set_result(HEREDOC_CONTENT as u16);
                    return true;
                }
                return false;
            }
            if interp && c == '#' {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '{' {
                    if has_content {
                        lx.set_result(HEREDOC_CONTENT as u16);
                        return true;
                    }
                    return false;
                }
                if scan_short_interpolation(lx, has_content, HEREDOC_CONTENT) {
                    return true;
                }
            } else if matches!(c, '\r' | '\n') {
                if c == '\r' {
                    lx.next();
                    if lx.lookahead() == '\n' {
                        lx.next();
                    }
                } else {
                    lx.next();
                }
                has_content = true;
                look_end = true;
                while matches!(lx.lookahead(), ' ' | '\t') {
                    lx.next();
                    if !indent_ok {
                        look_end = false;
                    }
                }
                lx.mark_end();
            } else {
                has_content = true;
                lx.next();
                lx.mark_end();
            }
        }
    }

    fn literal_content(&mut self, lx: &mut ScanLexer) -> bool {
        let last = self.literals.len() - 1;
        let mut has_content = false;
        let stop_on_space = matches!(
            self.literals[last].kind as usize,
            SYMBOL_ARRAY_START | STRING_ARRAY_START
        );
        loop {
            let c = lx.lookahead();
            if stop_on_space && is_space(c) {
                if has_content {
                    lx.mark_end();
                    lx.set_result(STRING_CONTENT as u16);
                    return true;
                }
                return false;
            }
            let lit = &mut self.literals[last];
            if c as i32 == lit.close {
                lx.mark_end();
                if lit.depth == 1 {
                    if has_content {
                        lx.set_result(STRING_CONTENT as u16);
                    } else {
                        lx.next();
                        if lit.kind as usize == REGEX_START {
                            while lx.lookahead().is_lowercase() {
                                lx.next();
                            }
                        }
                        self.literals.pop();
                        lx.set_result(STRING_END as u16);
                        lx.mark_end();
                    }
                    return true;
                }
                lit.depth -= 1;
                lx.next();
            } else if c as i32 == lit.open {
                lit.depth += 1;
                lx.next();
            } else if lit.interp && c == '#' {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '{' {
                    if has_content {
                        lx.set_result(STRING_CONTENT as u16);
                        return true;
                    }
                    return false;
                }
                if scan_short_interpolation(lx, has_content, STRING_CONTENT) {
                    return true;
                }
            } else if c == '\\' {
                if lit.interp {
                    if has_content {
                        lx.mark_end();
                        lx.set_result(STRING_CONTENT as u16);
                        return true;
                    }
                    return false;
                }
                lx.next();
                lx.next();
            } else if lx.eof() {
                lx.next();
                lx.mark_end();
                return false;
            } else {
                lx.next();
            }
            has_content = true;
        }
    }
}

impl Ruby {
    fn run(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        self.leading_ws = false;
        if !v[STRING_START] {
            if (v[STRING_CONTENT] || v[STRING_END]) && !self.literals.is_empty() {
                return self.literal_content(lx);
            }
            if (v[HEREDOC_CONTENT] || v[HEREDOC_BODY_END]) && !self.heredocs.is_empty() {
                return self.heredoc_content(lx);
            }
        }
        lx.set_result(NONE);
        if !self.scan_whitespace(lx, v) {
            return false;
        }
        if lx.result != NONE {
            return true;
        }
        let ws = self.leading_ws;
        match lx.lookahead() {
            '&' if v[BLOCK_AMPERSAND] => {
                lx.next();
                if !matches!(lx.lookahead(), '&' | '.' | '=') && !is_space(lx.lookahead()) {
                    lx.set_result(BLOCK_AMPERSAND as u16);
                    return true;
                }
                return false;
            }
            '<' if v[SINGLETON_CLASS_LEFT_ANGLE_LEFT_ANGLE] => {
                lx.next();
                if lx.lookahead() == '<' {
                    lx.next();
                    lx.set_result(SINGLETON_CLASS_LEFT_ANGLE_LEFT_ANGLE as u16);
                    return true;
                }
                return false;
            }
            '*' if v[SPLAT_STAR]
                || v[BINARY_STAR]
                || v[HASH_SPLAT_STAR_STAR]
                || v[BINARY_STAR_STAR] =>
            {
                lx.next();
                if lx.lookahead() == '=' {
                    return false;
                }
                if lx.lookahead() == '*' {
                    if v[HASH_SPLAT_STAR_STAR] || v[BINARY_STAR_STAR] {
                        lx.next();
                        if lx.lookahead() == '=' {
                            return false;
                        }
                        let r = if v[BINARY_STAR_STAR] && !ws {
                            BINARY_STAR_STAR
                        } else if v[HASH_SPLAT_STAR_STAR] && !is_space(lx.lookahead()) {
                            HASH_SPLAT_STAR_STAR
                        } else if v[BINARY_STAR_STAR] {
                            BINARY_STAR_STAR
                        } else if v[HASH_SPLAT_STAR_STAR] {
                            HASH_SPLAT_STAR_STAR
                        } else {
                            return false;
                        };
                        lx.set_result(r as u16);
                        return true;
                    }
                    return false;
                }
                let r = if v[BINARY_STAR] && !ws {
                    BINARY_STAR
                } else if v[SPLAT_STAR] && !is_space(lx.lookahead()) {
                    SPLAT_STAR
                } else if v[BINARY_STAR] {
                    BINARY_STAR
                } else if v[SPLAT_STAR] {
                    SPLAT_STAR
                } else {
                    return false;
                };
                lx.set_result(r as u16);
                return true;
            }
            '-' if v[UNARY_MINUS] || v[UNARY_MINUS_NUM] || v[BINARY_MINUS] => {
                lx.next();
                if !matches!(lx.lookahead(), '=' | '>') {
                    if v[UNARY_MINUS_NUM]
                        && (!v[BINARY_STAR] || ws)
                        && lx.lookahead().is_ascii_digit()
                    {
                        lx.set_result(UNARY_MINUS_NUM as u16);
                        return true;
                    }
                    let r = if v[UNARY_MINUS] && ws && !is_space(lx.lookahead()) {
                        UNARY_MINUS
                    } else if v[BINARY_MINUS] {
                        BINARY_MINUS
                    } else {
                        UNARY_MINUS
                    };
                    lx.set_result(r as u16);
                    return true;
                }
                return false;
            }
            ':' if v[SYMBOL_START] => {
                let mut lit = Literal {
                    kind: SYMBOL_START as u8,
                    depth: 1,
                    ..Literal::default()
                };
                lx.next();
                match lx.lookahead() {
                    q @ ('"' | '\'') => {
                        lx.next();
                        lit.open = q as i32;
                        lit.close = q as i32;
                        lit.interp = q == '"';
                        self.literals.push(lit);
                        lx.set_result(SYMBOL_START as u16);
                        return true;
                    }
                    _ => {
                        if scan_symbol_identifier(lx) {
                            lx.set_result(SIMPLE_SYMBOL as u16);
                            return true;
                        }
                    }
                }
                return false;
            }
            '[' if v[ELEMENT_REFERENCE_BRACKET] && (!ws || !v[STRING_START]) => {
                lx.next();
                lx.set_result(ELEMENT_REFERENCE_BRACKET as u16);
                return true;
            }
            _ => {}
        }

        let c = lx.lookahead();
        if ((v[HASH_KEY_SYMBOL] || v[IDENTIFIER_SUFFIX]) && (is_alpha(c) || c == '_'))
            || (v[CONSTANT_SUFFIX] && c.is_uppercase())
        {
            let suffix = if c.is_uppercase() {
                CONSTANT_SUFFIX
            } else {
                IDENTIFIER_SUFFIX
            };
            while alnum(lx.lookahead()) || lx.lookahead() == '_' {
                lx.next();
            }
            if v[HASH_KEY_SYMBOL] && lx.lookahead() == ':' {
                lx.mark_end();
                lx.next();
                if lx.lookahead() != ':' {
                    lx.set_result(HASH_KEY_SYMBOL as u16);
                    return true;
                }
            } else if v[suffix] && lx.lookahead() == '!' {
                lx.next();
                if lx.lookahead() != '=' {
                    lx.set_result(suffix as u16);
                    return true;
                }
            }
            return false;
        }

        if v[STRING_START] {
            let mut lit = Literal {
                depth: 1,
                ..Literal::default()
            };
            if lx.lookahead() == '<' {
                lx.next();
                if lx.lookahead() != '<' {
                    return false;
                }
                lx.next();
                let mut h = Heredoc::default();
                if matches!(lx.lookahead(), '-' | '~') {
                    lx.next();
                    h.indent_ok = true;
                }
                scan_heredoc_word(lx, &mut h);
                if h.word.is_empty() {
                    return false;
                }
                self.heredocs.push(h);
                lx.set_result(HEREDOC_START as u16);
                return true;
            }
            if self.open_delimiter(lx, &mut lit, v) {
                lx.set_result(lit.kind as u16);
                self.literals.push(lit);
                return true;
            }
            return false;
        }
        false
    }
}

impl Scanner for Ruby {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        self.run(lx, v)
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        if self.literals.len() * 5 + 2 >= MAX_SERIALIZED {
            return;
        }
        out.push(self.literals.len() as u8);
        for l in &self.literals {
            out.extend_from_slice(&[
                l.kind,
                l.open as u8,
                l.close as u8,
                l.depth as u8,
                l.interp as u8,
            ]);
        }
        out.push(self.heredocs.len() as u8);
        for h in &self.heredocs {
            if out.len() + 3 + 4 + h.word.len() >= MAX_SERIALIZED {
                out.clear();
                return;
            }
            out.extend_from_slice(&[h.indent_ok as u8, h.interp as u8, h.started as u8]);
            out.extend_from_slice(&(h.word.len() as u32).to_le_bytes());
            out.extend_from_slice(&h.word);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.leading_ws = false;
        self.literals.clear();
        self.heredocs.clear();
        if d.is_empty() {
            return;
        }
        let mut p = 0usize;
        let n = d[p] as usize;
        p += 1;
        for _ in 0..n {
            if p + 5 > d.len() {
                return;
            }
            self.literals.push(Literal {
                kind: d[p],
                open: d[p + 1] as i32,
                close: d[p + 2] as i32,
                depth: d[p + 3] as i32,
                interp: d[p + 4] != 0,
            });
            p += 5;
        }
        if p >= d.len() {
            return;
        }
        let n = d[p] as usize;
        p += 1;
        for _ in 0..n {
            if p + 7 > d.len() {
                return;
            }
            let len = u32::from_le_bytes([d[p + 3], d[p + 4], d[p + 5], d[p + 6]]) as usize;
            let (a, b, c) = (d[p] != 0, d[p + 1] != 0, d[p + 2] != 0);
            p += 7;
            let len = len.min(d.len() - p);
            self.heredocs.push(Heredoc {
                word: d[p..p + len].to_vec(),
                indent_ok: a,
                interp: b,
                started: c,
            });
            p += len;
        }
    }
}
