//! Nix 外部扫描器，移植自 tree-sitter-nix 的 `src/scanner.c`（MIT，Copyright (c) 2019 Charles Strahan）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner};

const STRING_FRAGMENT: usize = 0;
const INDENTED_STRING_FRAGMENT: usize = 1;
const PATH_START: usize = 2;
const PATH_FRAGMENT: usize = 3;
const DOLLAR_ESCAPE: usize = 4;
const INDENTED_DOLLAR_ESCAPE: usize = 5;

#[derive(Default)]
pub struct Nix;

fn scan_dollar_escape(lx: &mut ScanLexer) -> bool {
    lx.set_result(DOLLAR_ESCAPE as u16);
    lx.next();
    lx.mark_end();
    lx.lookahead() == '$'
}

fn scan_indented_dollar_escape(lx: &mut ScanLexer) -> bool {
    lx.set_result(INDENTED_DOLLAR_ESCAPE as u16);
    lx.next();
    lx.mark_end();
    if lx.lookahead() == '$' {
        return true;
    }
    if lx.lookahead() == '\\' {
        lx.next();
        if lx.lookahead() == '$' {
            lx.mark_end();
            return true;
        }
    }
    false
}

/// 字符串里的字面片段；分隔符、插值和转义序列交给解析器，这里遇到就停。
fn scan_string_fragment(lx: &mut ScanLexer) -> bool {
    lx.set_result(STRING_FRAGMENT as u16);
    let mut has_content = false;
    loop {
        lx.mark_end();
        match lx.lookahead() {
            '"' | '\\' => return has_content,
            '$' => {
                lx.next();
                if lx.lookahead() == '{' {
                    return has_content;
                } else if lx.lookahead() != '"' && lx.lookahead() != '\\' {
                    // `$` 后面除 `"`、`\`、`{` 之外的字符都算内容：`$${` 不是插值，`$$${` 才是
                    lx.next();
                }
            }
            '\0' => return false,
            _ => lx.next(),
        }
        has_content = true;
    }
}

fn scan_indented_string_fragment(lx: &mut ScanLexer) -> bool {
    lx.set_result(INDENTED_STRING_FRAGMENT as u16);
    let mut has_content = false;
    loop {
        lx.mark_end();
        match lx.lookahead() {
            '$' => {
                lx.next();
                if lx.lookahead() == '{' {
                    return has_content;
                } else if lx.lookahead() != '\'' {
                    lx.next();
                }
            }
            '\'' => {
                lx.next();
                if lx.lookahead() == '\'' {
                    // 两个单引号总会截断当前片段：结束符 `''` 或转义 `'''`、`''$`、`''\x`
                    return has_content;
                }
            }
            '\0' => return false,
            _ => lx.next(),
        }
        has_content = true;
    }
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '_' | '.' | '/')
}

fn scan_path_start(lx: &mut ScanLexer) -> bool {
    lx.set_result(PATH_START as u16);
    let (mut have_sep, mut have_after_sep) = (false, false);
    while matches!(lx.lookahead(), ' ' | '\n' | '\r' | '\t') {
        lx.skip();
    }
    loop {
        lx.mark_end();
        let c = lx.lookahead();
        if c == '/' {
            have_sep = true;
        } else if is_path_char(c) {
            if have_sep {
                have_after_sep = true;
            }
        } else if c == '$' {
            return have_sep;
        } else {
            return have_after_sep;
        }
        lx.next();
    }
}

fn scan_path_fragment(lx: &mut ScanLexer) -> bool {
    lx.set_result(PATH_FRAGMENT as u16);
    let mut has_content = false;
    loop {
        lx.mark_end();
        if !is_path_char(lx.lookahead()) {
            return has_content;
        }
        lx.next();
        has_content = true;
    }
}

impl Scanner for Nix {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        // 错误恢复时所有符号都有效；此时不能把内容吃成字符串片段
        if v[STRING_FRAGMENT]
            && v[INDENTED_STRING_FRAGMENT]
            && v[PATH_START]
            && v[PATH_FRAGMENT]
            && v[DOLLAR_ESCAPE]
            && v[INDENTED_DOLLAR_ESCAPE]
        {
            return false;
        }
        if v[STRING_FRAGMENT] {
            if lx.lookahead() == '\\' {
                return scan_dollar_escape(lx);
            }
            return scan_string_fragment(lx);
        }
        if v[INDENTED_STRING_FRAGMENT] {
            if lx.lookahead() == '\'' {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '\'' {
                    return scan_indented_dollar_escape(lx);
                }
            }
            return scan_indented_string_fragment(lx);
        }
        if v[PATH_FRAGMENT] && is_path_char(lx.lookahead()) {
            return scan_path_fragment(lx);
        }
        if v[PATH_START] {
            return scan_path_start(lx);
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
