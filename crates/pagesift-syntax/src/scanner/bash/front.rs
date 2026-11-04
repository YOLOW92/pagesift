//! Bash 扫描器的前段分支（拆自 `bash.rs`，逻辑不变）。

use super::*;

impl Bash {
    /// `regex:` 标号之前的全部分支。
    pub(super) fn front(&mut self, lx: &mut ScanLexer, v: &[bool]) -> Nx {
        let rec = v[ERROR_RECOVERY];
        if v[CONCAT] && !rec {
            let c = lx.lookahead();
            if !(c == '\0'
                || ws(c)
                || matches!(c, '>' | '<' | ')' | '(' | ';' | '&' | '|')
                || (c == '}' && v[CLOSING_BRACE])
                || (c == ']' && v[CLOSING_BRACKET]))
            {
                lx.set_result(CONCAT as u16);
                if c == '`' {
                    lx.mark_end();
                    lx.next();
                    while lx.lookahead() != '`' && !lx.eof() {
                        lx.next();
                    }
                    if lx.eof() {
                        return Nx::Ret(false);
                    }
                    if lx.lookahead() == '`' {
                        lx.next();
                    }
                    return Nx::Ret(ws(lx.lookahead()) || lx.eof());
                }
                if c == '\\' {
                    lx.mark_end();
                    lx.next();
                    if matches!(lx.lookahead(), '"' | '\'' | '\\') {
                        return Nx::Ret(true);
                    }
                    if lx.eof() {
                        return Nx::Ret(false);
                    }
                } else {
                    return Nx::Ret(true);
                }
            }
            if ws(lx.lookahead()) && v[CLOSING_BRACE] && !v[EXPANSION_WORD] {
                lx.set_result(CONCAT as u16);
                return Nx::Ret(true);
            }
        }
        if v[IMMEDIATE_DOUBLE_HASH] && !rec && lx.lookahead() == '#' {
            lx.mark_end();
            lx.next();
            if lx.lookahead() == '#' {
                lx.next();
                if lx.lookahead() != '}' {
                    lx.set_result(IMMEDIATE_DOUBLE_HASH as u16);
                    lx.mark_end();
                    return Nx::Ret(true);
                }
            }
        }
        if v[EXTERNAL_EXPANSION_SYM_HASH] && !rec && matches!(lx.lookahead(), '#' | '=' | '!') {
            lx.set_result(match lx.lookahead() {
                '#' => EXTERNAL_EXPANSION_SYM_HASH,
                '!' => EXTERNAL_EXPANSION_SYM_BANG,
                _ => EXTERNAL_EXPANSION_SYM_EQUAL,
            } as u16);
            lx.next();
            lx.mark_end();
            while matches!(lx.lookahead(), '#' | '=' | '!') {
                lx.next();
            }
            while ws(lx.lookahead()) {
                lx.skip();
            }
            return Nx::Ret(lx.lookahead() == '}');
        }
        if v[EMPTY_VALUE]
            && (ws(lx.lookahead()) || lx.eof() || lx.lookahead() == ';' || lx.lookahead() == '&')
        {
            lx.set_result(EMPTY_VALUE as u16);
            return Nx::Ret(true);
        }
        if (v[HEREDOC_BODY_BEGINNING] || v[SIMPLE_HEREDOC_BODY])
            && !self.heredocs.is_empty()
            && !self.heredocs.last().unwrap().started
            && !rec
        {
            return Nx::Ret(self.heredoc_content(lx, HEREDOC_BODY_BEGINNING, SIMPLE_HEREDOC_BODY));
        }
        if v[HEREDOC_END] && !self.heredocs.is_empty() {
            let last = self.heredocs.len() - 1;
            if heredoc_end_identifier(&mut self.heredocs[last], lx) {
                self.heredocs.pop();
                lx.set_result(HEREDOC_END as u16);
                return Nx::Ret(true);
            }
        }
        if v[HEREDOC_CONTENT]
            && !self.heredocs.is_empty()
            && self.heredocs.last().unwrap().started
            && !rec
        {
            return Nx::Ret(self.heredoc_content(lx, HEREDOC_CONTENT, HEREDOC_END));
        }
        if v[HEREDOC_START] && !rec && !self.heredocs.is_empty() {
            let last = self.heredocs.len() - 1;
            return Nx::Ret(heredoc_start(&mut self.heredocs[last], lx));
        }
        if v[TEST_OPERATOR] && !v[EXPANSION_WORD] {
            while ws(lx.lookahead()) && lx.lookahead() != '\n' {
                lx.skip();
            }
            if lx.lookahead() == '\\' {
                if v[EXTGLOB_PATTERN] {
                    return Nx::Go(St::Extglob);
                }
                if v[REGEX_NO_SPACE] {
                    return Nx::Go(St::Regex);
                }
                lx.skip();
                if lx.eof() {
                    return Nx::Ret(false);
                }
                if lx.lookahead() == '\r' {
                    lx.skip();
                    if lx.lookahead() == '\n' {
                        lx.skip();
                    }
                } else if lx.lookahead() == '\n' {
                    lx.skip();
                } else {
                    return Nx::Ret(false);
                }
                while ws(lx.lookahead()) {
                    lx.skip();
                }
            }
            if lx.lookahead() == '\n' && !v[NEWLINE] {
                lx.skip();
                while ws(lx.lookahead()) {
                    lx.skip();
                }
            }
            if lx.lookahead() == '-' {
                lx.next();
                let mut advanced_once = false;
                while alpha(lx.lookahead()) {
                    advanced_once = true;
                    lx.next();
                }
                if ws(lx.lookahead()) && advanced_once {
                    lx.mark_end();
                    lx.next();
                    if lx.lookahead() == '}' && v[CLOSING_BRACE] {
                        if v[EXPANSION_WORD] {
                            lx.mark_end();
                            lx.set_result(EXPANSION_WORD as u16);
                            return Nx::Ret(true);
                        }
                        return Nx::Ret(false);
                    }
                    lx.set_result(TEST_OPERATOR as u16);
                    return Nx::Ret(true);
                }
                if ws(lx.lookahead()) && v[EXTGLOB_PATTERN] {
                    lx.set_result(EXTGLOB_PATTERN as u16);
                    return Nx::Ret(true);
                }
            }
            if v[BARE_DOLLAR] && !rec && bare_dollar(lx) {
                return Nx::Ret(true);
            }
        }
        if (v[VARIABLE_NAME] || v[FILE_DESCRIPTOR] || v[HEREDOC_ARROW])
            && !v[REGEX_NO_SLASH]
            && !rec
        {
            loop {
                let c = lx.lookahead();
                if (matches!(c, ' ' | '\t' | '\r') || (c == '\n' && !v[NEWLINE]))
                    && !v[EXPANSION_WORD]
                {
                    lx.skip();
                } else if c == '\\' {
                    lx.skip();
                    if lx.eof() {
                        lx.mark_end();
                        lx.set_result(VARIABLE_NAME as u16);
                        return Nx::Ret(true);
                    }
                    if lx.lookahead() == '\r' {
                        lx.skip();
                    }
                    if lx.lookahead() == '\n' {
                        lx.skip();
                    } else {
                        if lx.lookahead() == '\\' && v[EXPANSION_WORD] {
                            return Nx::Go(St::Expansion);
                        }
                        return Nx::Ret(false);
                    }
                } else {
                    break;
                }
            }
            if !v[EXPANSION_WORD] && matches!(lx.lookahead(), '*' | '@' | '?' | '-' | '0' | '_') {
                lx.mark_end();
                lx.next();
                if matches!(lx.lookahead(), '=' | '[' | ':' | '-' | '%' | '#' | '/') {
                    return Nx::Ret(false);
                }
                if v[EXTGLOB_PATTERN] && ws(lx.lookahead()) {
                    lx.mark_end();
                    lx.set_result(EXTGLOB_PATTERN as u16);
                    return Nx::Ret(true);
                }
            }
            if v[HEREDOC_ARROW] && lx.lookahead() == '<' {
                lx.next();
                if lx.lookahead() == '<' {
                    lx.next();
                    if lx.lookahead() == '-' {
                        lx.next();
                        self.heredocs.push(Heredoc {
                            allows_indent: true,
                            ..Default::default()
                        });
                        lx.set_result(HEREDOC_ARROW_DASH as u16);
                    } else if lx.lookahead() == '<' || lx.lookahead() == '=' {
                        return Nx::Ret(false);
                    } else {
                        self.heredocs.push(Heredoc::default());
                        lx.set_result(HEREDOC_ARROW as u16);
                    }
                    return Nx::Ret(true);
                }
                return Nx::Ret(false);
            }
            let mut is_number = true;
            if lx.lookahead().is_ascii_digit() {
                lx.next();
            } else if alpha(lx.lookahead()) || lx.lookahead() == '_' {
                is_number = false;
                lx.next();
            } else {
                if lx.lookahead() == '{' {
                    return Nx::Go(St::Brace);
                }
                if v[EXPANSION_WORD] {
                    return Nx::Go(St::Expansion);
                }
                if v[EXTGLOB_PATTERN] {
                    return Nx::Go(St::Extglob);
                }
                return Nx::Ret(false);
            }
            loop {
                if lx.lookahead().is_ascii_digit() {
                    lx.next();
                } else if alpha(lx.lookahead()) || lx.lookahead() == '_' {
                    is_number = false;
                    lx.next();
                } else {
                    break;
                }
            }
            if is_number && v[FILE_DESCRIPTOR] && (lx.lookahead() == '>' || lx.lookahead() == '<') {
                lx.set_result(FILE_DESCRIPTOR as u16);
                return Nx::Ret(true);
            }
            if v[VARIABLE_NAME] {
                if lx.lookahead() == '+' {
                    lx.mark_end();
                    lx.next();
                    if lx.lookahead() == '=' || lx.lookahead() == ':' || v[CLOSING_BRACE] {
                        lx.set_result(VARIABLE_NAME as u16);
                        return Nx::Ret(true);
                    }
                    return Nx::Ret(false);
                }
                if lx.lookahead() == '/' {
                    return Nx::Ret(false);
                }
                let c = lx.lookahead();
                if c == '='
                    || c == '['
                    || (c == ':' && !v[CLOSING_BRACE] && !v[OPENING_PAREN])
                    || c == '%'
                    || (c == '#' && !is_number)
                    || c == '@'
                    || (c == '-' && v[CLOSING_BRACE])
                {
                    lx.mark_end();
                    lx.set_result(VARIABLE_NAME as u16);
                    return Nx::Ret(true);
                }
                if c == '?' {
                    lx.mark_end();
                    lx.next();
                    lx.set_result(VARIABLE_NAME as u16);
                    return Nx::Ret(alpha(lx.lookahead()));
                }
            }
            return Nx::Ret(false);
        }
        if v[BARE_DOLLAR] && !rec && bare_dollar(lx) {
            return Nx::Ret(true);
        }
        Nx::Go(St::Regex)
    }
}
