//! Kotlin 扫描器的自动分号与构造器提示分支（拆自 `probe.rs`，逻辑不变）。

use super::probe::*;
use super::*;

impl Kotlin {
    pub(super) fn scan_primary_constructor_hint(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let mut newline = self.constructor_newline;
        self.constructor_newline = false;
        while constructor_space(lx.lookahead()) {
            if matches!(lx.lookahead(), '\n' | '\r') {
                newline = true;
            }
            lx.skip();
        }
        if lx.lookahead() == '/' {
            if v[MULTILINE_COMMENT] && scan_multiline_comment(lx, &mut newline, &mut 0) {
                self.constructor_newline = newline;
                return true;
            }
            return false;
        }
        lx.mark_end();
        if lx.lookahead() == ';' {
            lx.next();
            lx.mark_end();
            lx.set_result(AUTOMATIC_SEMICOLON as u16);
            return v[AUTOMATIC_SEMICOLON];
        }
        if lx.eof() {
            lx.set_result(AUTOMATIC_SEMICOLON as u16);
            return v[AUTOMATIC_SEMICOLON];
        }
        let header_newline = newline;
        let first = lx.lookahead();
        let mut first_word: Vec<u8> = Vec::new();
        let mut first_identifier = true;
        loop {
            if lx.lookahead() == '@' {
                if !constructor_annotation(lx) {
                    break;
                }
            } else {
                let (word, ascii) = constructor_word(lx, 32);
                if first_identifier {
                    first_word = word.clone();
                    first_identifier = false;
                }
                if !ascii {
                    break;
                }
                if word == b"constructor" {
                    if constructor_trivia(lx, &mut newline) && lx.lookahead() == '(' {
                        lx.set_result(PRIMARY_CONSTRUCTOR_HINT as u16);
                        return true;
                    }
                    break;
                }
                if !constructor_modifier(&word) {
                    break;
                }
            }
            if !constructor_trivia(lx, &mut newline) {
                break;
            }
        }
        let fw = first_word.as_slice();
        let mut continuation = matches!(
            first,
            '(' | '{' | '<' | '.' | '?' | ',' | '*' | '%' | '>' | '=' | '|' | '&'
        ) || fw == b"as"
            || (v[BY_DELEGATION_HINT] && fw == b"by");
        if fw == b"where" {
            continuation = constructor_trivia(lx, &mut newline) && lx.lookahead() != '(';
        }
        if first == ':' {
            lx.next();
            continuation = lx.lookahead() != ':';
        }
        if fw == b"finally" {
            continuation = v[TRY_CLAUSE_HINT];
        }
        if first == '!' {
            lx.next();
            continuation = lx.lookahead() == '=';
        }
        if fw == b"else" {
            continuation = true;
            if constructor_trivia(lx, &mut newline) && lx.lookahead() == '-' {
                lx.next();
                continuation = lx.lookahead() != '>';
            }
        }
        lx.set_result(AUTOMATIC_SEMICOLON as u16);
        (if header_newline {
            !continuation
        } else {
            fw == b"import"
        }) && v[AUTOMATIC_SEMICOLON]
    }

    pub(super) fn resume_pending_comment(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if self.comment.semi_owed {
            if !v[AUTOMATIC_SEMICOLON] {
                return false;
            }
            lx.mark_end();
            lx.set_result(AUTOMATIC_SEMICOLON as u16);
            self.comment.semi_owed = false;
            return true;
        }
        if !v[MULTILINE_COMMENT] {
            return false;
        }
        let length = self.comment.end - self.comment.start;
        let mut consumed = 0u32;
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        let matched = scan_multiline_comment(lx, &mut false, &mut consumed);
        self.comment = PendingComment::default();
        matched && consumed == length
    }

    /// `clean` 只在返回假时有意义：光标是否仍停在第一个非空白字符上。
    pub(super) fn scan_automatic_semicolon(
        &mut self,
        lx: &mut ScanLexer,
        v: &[bool],
        clean: &mut bool,
    ) -> bool {
        lx.set_result(AUTOMATIC_SEMICOLON as u16);
        lx.mark_end();
        let mut prefix_bytes = 0u32;
        let mut sameline = true;
        loop {
            if lx.eof() {
                return true;
            }
            if lx.lookahead() == ';' {
                lx.next();
                lx.mark_end();
                return true;
            }
            if !is_space(lx.lookahead()) {
                break;
            }
            if lx.lookahead() == '\n' {
                advance_counted(lx, &mut prefix_bytes, true);
                sameline = false;
                break;
            }
            if lx.lookahead() == '\r' {
                advance_counted(lx, &mut prefix_bytes, true);
                if lx.lookahead() == '\n' {
                    advance_counted(lx, &mut prefix_bytes, true);
                }
                sameline = false;
                break;
            }
            advance_counted(lx, &mut prefix_bytes, true);
        }
        while is_space(lx.lookahead()) {
            advance_counted(lx, &mut prefix_bytes, true);
        }
        if sameline {
            return match lx.lookahead() {
                'i' => scan_for_word(lx, "mport"),
                ';' => {
                    lx.next();
                    lx.mark_end();
                    true
                }
                _ => {
                    *clean = true;
                    false
                }
            };
        }
        let guard = !v[STRING_CONTENT];
        match lx.lookahead() {
            ',' | '.' | '*' | '%' | '>' | '<' | '=' | '{' | '?' | '|' | '&' => {
                *clean = true;
                false
            }
            ':' => {
                lx.skip();
                lx.lookahead() == ':'
            }
            '/' => {
                lx.next();
                if lx.lookahead() == '/' {
                    lx.skip();
                    while !matches!(lx.lookahead(), '\n' | '\r' | '\0') && !lx.eof() {
                        lx.skip();
                    }
                    if !skip_ws_comments(lx) {
                        return false;
                    }
                    return asi_after_comment(lx, v);
                } else if lx.lookahead() == '*' {
                    lx.next();
                    let mut comment_bytes = 2u32;
                    let mut depth = 1u32;
                    let (mut nested, mut has_nul) = (false, false);
                    while depth > 0 && !lx.eof() {
                        if lx.lookahead() == '*' {
                            advance_counted(lx, &mut comment_bytes, false);
                            if lx.lookahead() == '/' {
                                advance_counted(lx, &mut comment_bytes, false);
                                depth -= 1;
                            }
                        } else if lx.lookahead() == '/' {
                            advance_counted(lx, &mut comment_bytes, false);
                            if lx.lookahead() == '*' {
                                advance_counted(lx, &mut comment_bytes, false);
                                depth += 1;
                                nested = true;
                            }
                        } else {
                            if lx.lookahead() == '\0' {
                                has_nul = true;
                            }
                            advance_counted(lx, &mut comment_bytes, false);
                        }
                    }
                    if depth > 0 {
                        return true;
                    }
                    if nested || has_nul {
                        if continuation_after_block_comment(lx, v)
                            && v[COMMENT_CONTINUATION_HINT]
                            && !v[STRING_CONTENT]
                            && prefix_bytes <= u32::MAX - comment_bytes
                        {
                            self.comment = PendingComment {
                                pending: true,
                                start: prefix_bytes,
                                end: prefix_bytes + comment_bytes,
                                semi_owed: false,
                            };
                            lx.set_result(COMMENT_CONTINUATION_HINT as u16);
                        }
                        return true;
                    }
                    if !skip_ws_comments(lx) {
                        return false;
                    }
                    return asi_after_comment(lx, v);
                }
                false
            }
            '+' | '-' => true,
            '!' => {
                lx.skip();
                lx.lookahead() != '='
            }
            'b' => !(v[BY_DELEGATION_HINT] && scan_for_word(lx, "y")),
            'e' => {
                if !scan_for_word(lx, "lse") {
                    return true;
                }
                followed_by_arrow(lx)
            }
            'a' => !scan_for_word(lx, "s"),
            'w' => {
                if !scan_for_word(lx, "here") {
                    return true;
                }
                followed_by(lx, '(')
            }
            'i' | 'p' => {
                if v[PRIMARY_CONSTRUCTOR_KEYWORD] && guard && check_modifier_then(lx, "constructor")
                {
                    return false;
                }
                if v[BACKING_FIELD_HINT] && guard && check_modifier_then(lx, "field") {
                    return false;
                }
                true
            }
            'c' => {
                if v[PRIMARY_CONSTRUCTOR_KEYWORD] && guard {
                    if eat_constructor(lx) {
                        lx.set_result(PRIMARY_CONSTRUCTOR_KEYWORD as u16);
                        lx.mark_end();
                    }
                    return true;
                }
                !(v[TRY_CLAUSE_HINT] && scan_try_clause_keyword(lx, "atch"))
            }
            'f' => {
                if v[BACKING_FIELD_HINT] && scan_try_clause_keyword(lx, "ield") {
                    return false;
                }
                !(v[TRY_CLAUSE_HINT] && scan_try_clause_keyword(lx, "inally"))
            }
            '@' => {
                if v[PRIMARY_CONSTRUCTOR_KEYWORD]
                    && guard
                    && check_annotation_then(lx, "constructor")
                {
                    return false;
                }
                if v[BACKING_FIELD_HINT] && guard && check_annotation_then(lx, "field") {
                    return false;
                }
                true
            }
            ';' => {
                lx.next();
                lx.mark_end();
                true
            }
            _ => true,
        }
    }
}
