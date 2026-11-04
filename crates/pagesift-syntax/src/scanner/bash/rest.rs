//! Bash 扫描器的正则、扩展 glob、参数扩展和花括号分支（拆自 `bash.rs`，逻辑不变）。

use super::*;

impl Bash {
    pub(super) fn regex(&mut self, lx: &mut ScanLexer, v: &[bool]) -> Nx {
        let _ = &self;
        if !((v[REGEX] || v[REGEX_NO_SLASH] || v[REGEX_NO_SPACE]) && !v[ERROR_RECOVERY]) {
            return Nx::Go(St::Extglob);
        }
        if v[REGEX] || v[REGEX_NO_SPACE] {
            while ws(lx.lookahead()) {
                lx.skip();
            }
        }
        let c = lx.lookahead();
        if (c != '"' && c != '\'')
            || ((c == '$' || c == '\'') && v[REGEX_NO_SLASH])
            || (c == '\'' && v[REGEX_NO_SPACE])
        {
            if c == '$' && v[REGEX_NO_SLASH] {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '(' {
                    return Nx::Ret(false);
                }
            }
            lx.mark_end();
            let (mut done, mut advanced_once, mut found_non_alnum, mut last_escape, mut in_sq) =
                (false, false, false, false, false);
            let (mut paren, mut bracket, mut brace) = (0u32, 0u32, 0u32);
            while !done {
                if in_sq && lx.lookahead() == '\'' {
                    in_sq = false;
                    lx.next();
                    lx.mark_end();
                }
                match lx.lookahead() {
                    '\\' => last_escape = true,
                    '\0' => return Nx::Ret(false),
                    '(' => {
                        paren = paren.wrapping_add(1);
                        last_escape = false;
                    }
                    '[' => {
                        bracket = bracket.wrapping_add(1);
                        last_escape = false;
                    }
                    '{' => {
                        if !last_escape {
                            brace = brace.wrapping_add(1);
                        }
                        last_escape = false;
                    }
                    ')' => {
                        if paren == 0 {
                            done = true;
                        }
                        paren = paren.wrapping_sub(1);
                        last_escape = false;
                    }
                    ']' => {
                        if bracket == 0 {
                            done = true;
                        }
                        bracket = bracket.wrapping_sub(1);
                        last_escape = false;
                    }
                    '}' => {
                        if brace == 0 {
                            done = true;
                        }
                        brace = brace.wrapping_sub(1);
                        last_escape = false;
                    }
                    '\'' => {
                        in_sq = !in_sq;
                        lx.next();
                        advanced_once = true;
                        last_escape = false;
                        continue;
                    }
                    _ => last_escape = false,
                }
                if !done {
                    if v[REGEX] {
                        let was_space = !in_sq && ws(lx.lookahead());
                        lx.next();
                        advanced_once = true;
                        if !was_space || paren > 0 {
                            lx.mark_end();
                        }
                    } else if v[REGEX_NO_SLASH] {
                        if lx.lookahead() == '/' {
                            lx.mark_end();
                            lx.set_result(REGEX_NO_SLASH as u16);
                            return Nx::Ret(advanced_once);
                        }
                        if lx.lookahead() == '\\' {
                            lx.next();
                            advanced_once = true;
                            if !lx.eof() && lx.lookahead() != '[' && lx.lookahead() != '/' {
                                lx.next();
                                lx.mark_end();
                            }
                        } else {
                            let was_space = !in_sq && ws(lx.lookahead());
                            lx.next();
                            advanced_once = true;
                            if !was_space {
                                lx.mark_end();
                            }
                        }
                    } else if v[REGEX_NO_SPACE] {
                        if lx.lookahead() == '\\' {
                            found_non_alnum = true;
                            lx.next();
                            if !lx.eof() {
                                lx.next();
                            }
                        } else if lx.lookahead() == '$' {
                            lx.mark_end();
                            lx.next();
                            if lx.lookahead() == '(' {
                                return Nx::Ret(false);
                            }
                            if ws(lx.lookahead()) {
                                lx.set_result(REGEX_NO_SPACE as u16);
                                lx.mark_end();
                                return Nx::Ret(true);
                            }
                        } else {
                            let was_space = !in_sq && ws(lx.lookahead());
                            if was_space && paren == 0 {
                                lx.mark_end();
                                lx.set_result(REGEX_NO_SPACE as u16);
                                return Nx::Ret(found_non_alnum);
                            }
                            if !alnum(lx.lookahead())
                                && lx.lookahead() != '$'
                                && lx.lookahead() != '-'
                                && lx.lookahead() != '_'
                            {
                                found_non_alnum = true;
                            }
                            lx.next();
                        }
                    }
                }
            }
            lx.set_result(if v[REGEX_NO_SLASH] {
                REGEX_NO_SLASH
            } else if v[REGEX_NO_SPACE] {
                REGEX_NO_SPACE
            } else {
                REGEX
            } as u16);
            if v[REGEX] && !advanced_once {
                return Nx::Ret(false);
            }
            return Nx::Ret(true);
        }
        Nx::Go(St::Extglob)
    }

    pub(super) fn extglob(&mut self, lx: &mut ScanLexer, v: &[bool]) -> Nx {
        if !(v[EXTGLOB_PATTERN] && !v[ERROR_RECOVERY]) {
            return Nx::Go(St::Expansion);
        }
        while ws(lx.lookahead()) {
            lx.skip();
        }
        let c = lx.lookahead();
        if matches!(
            c,
            '?' | '*' | '+' | '@' | '!' | '-' | ')' | '\\' | '.' | '['
        ) || alpha(c)
        {
            if lx.lookahead() == '\\' {
                lx.next();
                if (ws(lx.lookahead()) || lx.lookahead() == '"')
                    && lx.lookahead() != '\r'
                    && lx.lookahead() != '\n'
                {
                    lx.next();
                } else {
                    return Nx::Ret(false);
                }
            }
            if lx.lookahead() == ')' && self.last_glob_paren_depth == 0 {
                lx.mark_end();
                lx.next();
                if ws(lx.lookahead()) {
                    return Nx::Ret(false);
                }
            }
            lx.mark_end();
            let was_non_alpha = !alpha(lx.lookahead());
            if lx.lookahead() != '[' {
                if lx.lookahead() == 'e' {
                    lx.mark_end();
                    lx.next();
                    if lx.lookahead() == 's' {
                        lx.next();
                        if lx.lookahead() == 'a' {
                            lx.next();
                            if lx.lookahead() == 'c' {
                                lx.next();
                                if ws(lx.lookahead()) {
                                    return Nx::Ret(false);
                                }
                            }
                        }
                    }
                } else {
                    lx.next();
                }
            }
            if lx.lookahead() == '-' {
                lx.mark_end();
                lx.next();
                while alnum(lx.lookahead()) {
                    lx.next();
                }
                if matches!(lx.lookahead(), ')' | '\\' | '.') {
                    return Nx::Ret(false);
                }
                lx.mark_end();
            }
            if lx.lookahead() == ')' && self.last_glob_paren_depth == 0 {
                lx.mark_end();
                lx.next();
                if ws(lx.lookahead()) {
                    lx.set_result(EXTGLOB_PATTERN as u16);
                    return Nx::Ret(was_non_alpha);
                }
            }
            if ws(lx.lookahead()) {
                lx.mark_end();
                lx.set_result(EXTGLOB_PATTERN as u16);
                self.last_glob_paren_depth = 0;
                return Nx::Ret(true);
            }
            if lx.lookahead() == '$' {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '{' || lx.lookahead() == '(' {
                    lx.set_result(EXTGLOB_PATTERN as u16);
                    return Nx::Ret(true);
                }
            }
            if lx.lookahead() == '|' {
                lx.mark_end();
                lx.next();
                lx.set_result(EXTGLOB_PATTERN as u16);
                return Nx::Ret(true);
            }
            let c = lx.lookahead();
            if !alnum(c) && !matches!(c, '(' | '"' | '[' | '?' | '/' | '\\' | '_' | '*') {
                return Nx::Ret(false);
            }
            let (mut done, mut saw_non_alphadot) = (false, was_non_alpha);
            let (mut paren, mut bracket, mut brace) =
                (self.last_glob_paren_depth as u32, 0u32, 0u32);
            while !done {
                match lx.lookahead() {
                    '\0' => return Nx::Ret(false),
                    '(' => paren = paren.wrapping_add(1),
                    '[' => bracket = bracket.wrapping_add(1),
                    '{' => brace = brace.wrapping_add(1),
                    ')' => {
                        if paren == 0 {
                            done = true;
                        }
                        paren = paren.wrapping_sub(1);
                    }
                    ']' => {
                        if bracket == 0 {
                            done = true;
                        }
                        bracket = bracket.wrapping_sub(1);
                    }
                    '}' => {
                        if brace == 0 {
                            done = true;
                        }
                        brace = brace.wrapping_sub(1);
                    }
                    _ => {}
                }
                if lx.lookahead() == '|' {
                    lx.mark_end();
                    lx.next();
                    if paren == 0 && bracket == 0 && brace == 0 {
                        lx.set_result(EXTGLOB_PATTERN as u16);
                        return Nx::Ret(true);
                    }
                }
                if !done {
                    let was_space = ws(lx.lookahead());
                    if lx.lookahead() == '$' {
                        lx.mark_end();
                        if !alpha(lx.lookahead()) && lx.lookahead() != '.' && lx.lookahead() != '\\'
                        {
                            saw_non_alphadot = true;
                        }
                        lx.next();
                        if lx.lookahead() == '(' || lx.lookahead() == '{' {
                            lx.set_result(EXTGLOB_PATTERN as u16);
                            self.last_glob_paren_depth = paren as u8;
                            return Nx::Ret(saw_non_alphadot);
                        }
                    }
                    if was_space {
                        lx.mark_end();
                        lx.set_result(EXTGLOB_PATTERN as u16);
                        self.last_glob_paren_depth = 0;
                        return Nx::Ret(saw_non_alphadot);
                    }
                    if lx.lookahead() == '"' {
                        lx.mark_end();
                        lx.set_result(EXTGLOB_PATTERN as u16);
                        self.last_glob_paren_depth = 0;
                        return Nx::Ret(saw_non_alphadot);
                    }
                    if lx.lookahead() == '\\' {
                        if !alpha(lx.lookahead()) && lx.lookahead() != '.' && lx.lookahead() != '\\'
                        {
                            saw_non_alphadot = true;
                        }
                        lx.next();
                        if ws(lx.lookahead()) || lx.lookahead() == '"' {
                            lx.next();
                        }
                    } else {
                        if !alpha(lx.lookahead()) && lx.lookahead() != '.' && lx.lookahead() != '\\'
                        {
                            saw_non_alphadot = true;
                        }
                        lx.next();
                    }
                    if !was_space {
                        lx.mark_end();
                    }
                }
            }
            lx.set_result(EXTGLOB_PATTERN as u16);
            self.last_glob_paren_depth = 0;
            return Nx::Ret(saw_non_alphadot);
        }
        self.last_glob_paren_depth = 0;
        Nx::Ret(false)
    }

    pub(super) fn expansion_word(&mut self, lx: &mut ScanLexer, v: &[bool]) -> Nx {
        let _ = &self;
        if !v[EXPANSION_WORD] {
            return Nx::Go(St::Brace);
        }
        let (mut advanced_once, mut advance_once_space) = (false, false);
        loop {
            if lx.lookahead() == '"' {
                return Nx::Ret(false);
            }
            if lx.lookahead() == '$' {
                lx.mark_end();
                lx.next();
                if matches!(lx.lookahead(), '{' | '(' | '\'') || alnum(lx.lookahead()) {
                    lx.set_result(EXPANSION_WORD as u16);
                    return Nx::Ret(advanced_once);
                }
                advanced_once = true;
            }
            if lx.lookahead() == '}' {
                lx.mark_end();
                lx.set_result(EXPANSION_WORD as u16);
                return Nx::Ret(advanced_once || advance_once_space);
            }
            if lx.lookahead() == '(' && !(advanced_once || advance_once_space) {
                lx.mark_end();
                lx.next();
                while lx.lookahead() != ')' && !lx.eof() {
                    if lx.lookahead() == '$' {
                        lx.mark_end();
                        lx.next();
                        if matches!(lx.lookahead(), '{' | '(' | '\'') || alnum(lx.lookahead()) {
                            lx.set_result(EXPANSION_WORD as u16);
                            return Nx::Ret(advanced_once);
                        }
                        advanced_once = true;
                    } else {
                        advanced_once = advanced_once || !ws(lx.lookahead());
                        advance_once_space = advance_once_space || ws(lx.lookahead());
                        lx.next();
                    }
                }
                lx.mark_end();
                if lx.lookahead() == ')' {
                    advanced_once = true;
                    lx.next();
                    lx.mark_end();
                    if lx.lookahead() == '}' {
                        return Nx::Ret(false);
                    }
                } else {
                    return Nx::Ret(false);
                }
            }
            if lx.lookahead() == '\'' {
                return Nx::Ret(false);
            }
            if lx.eof() {
                return Nx::Ret(false);
            }
            advanced_once = advanced_once || !ws(lx.lookahead());
            advance_once_space = advance_once_space || ws(lx.lookahead());
            lx.next();
        }
    }

    pub(super) fn brace_start(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let _ = &self;
        if !(v[BRACE_START] && !v[ERROR_RECOVERY]) {
            return false;
        }
        while ws(lx.lookahead()) {
            lx.skip();
        }
        if lx.lookahead() != '{' {
            return false;
        }
        lx.next();
        lx.mark_end();
        while lx.lookahead().is_ascii_digit() {
            lx.next();
        }
        if lx.lookahead() != '.' {
            return false;
        }
        lx.next();
        if lx.lookahead() != '.' {
            return false;
        }
        lx.next();
        while lx.lookahead().is_ascii_digit() {
            lx.next();
        }
        if lx.lookahead() != '}' {
            return false;
        }
        lx.set_result(BRACE_START as u16);
        true
    }
}
