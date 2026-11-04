//! Ruby 扫描器的字面量与 here-doc 内容分支（拆自 `ruby.rs`，逻辑不变）。

use super::*;

impl Ruby {
    pub(super) fn open_delimiter(
        &mut self,
        lx: &mut ScanLexer,
        lit: &mut Literal,
        v: &[bool],
    ) -> bool {
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

    pub(super) fn heredoc_content(&mut self, lx: &mut ScanLexer) -> bool {
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

    pub(super) fn literal_content(&mut self, lx: &mut ScanLexer) -> bool {
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
