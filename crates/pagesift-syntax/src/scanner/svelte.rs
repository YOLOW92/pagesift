//! Svelte 外部扫描器的花括号文本部分，移植自 tree-sitter-svelte 的 `src/scanner.c`
//! （MIT，Copyright (c) 2024 Amaan Qureshi）。标签栈部分与 HTML 共用 `html.rs`（`Html::svelte()`）。

use super::{ScanLexer, is_space};

const SVELTE_RAW_TEXT: usize = 9;
const SVELTE_RAW_TEXT_EACH: usize = 10;
const SVELTE_RAW_TEXT_SNIPPET_ARGUMENTS: usize = 11;
const AT: usize = 12;
const HASH: usize = 13;
const SLASH: usize = 14;
const COLON: usize = 15;

/// 模板字符串和花括号会互相嵌套；输入不可信，限制递归深度。
const MAX_NEST: u32 = 64;

/// 已经读过 `/`、看到 `*` 之后，前进到块注释末尾。
fn js_block_comment(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() != '*' {
        return false;
    }
    lx.next();
    while lx.lookahead() != '\0' {
        if lx.lookahead() == '*' {
            lx.next();
            if lx.lookahead() == '/' {
                lx.next();
                return true;
            }
        } else {
            lx.next();
        }
    }
    false
}

fn js_line_comment(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() != '/' {
        return false;
    }
    lx.next();
    while lx.lookahead() != '\0' {
        let c = lx.lookahead();
        lx.next();
        if matches!(c, '\n' | '\r') {
            return true;
        }
    }
    false
}

fn js_quoted_string(lx: &mut ScanLexer, delim: char) -> bool {
    if lx.lookahead() != delim {
        return false;
    }
    lx.next();
    while lx.lookahead() != '\0' {
        if lx.lookahead() == '\\' {
            lx.next();
            lx.next();
        } else if lx.lookahead() == delim {
            lx.next();
            return true;
        } else {
            lx.next();
        }
    }
    false
}

fn js_balanced_brace(lx: &mut ScanLexer, nest: u32) -> bool {
    if lx.lookahead() != '{' {
        return false;
    }
    let mut level = 0u8;
    lx.next();
    while lx.lookahead() != '\0' {
        match lx.lookahead() {
            '`' => {
                if nest < MAX_NEST {
                    js_template_string(lx, nest + 1);
                } else {
                    lx.next();
                }
            }
            '\\' => {
                lx.next();
                lx.next();
            }
            c @ ('\'' | '"') => {
                js_quoted_string(lx, c);
            }
            '{' => {
                level = level.wrapping_add(1);
                lx.next();
            }
            '}' => {
                lx.next();
                if level == 0 {
                    return true;
                }
                level -= 1;
            }
            _ => lx.next(),
        }
    }
    false
}

fn js_template_string(lx: &mut ScanLexer, nest: u32) -> bool {
    if lx.lookahead() != '`' {
        return false;
    }
    lx.next();
    while lx.lookahead() != '\0' {
        match lx.lookahead() {
            '$' => {
                lx.next();
                if lx.lookahead() == '{' {
                    if nest < MAX_NEST {
                        js_balanced_brace(lx, nest + 1);
                    } else {
                        lx.next();
                    }
                }
            }
            '\\' => {
                lx.next();
                lx.next();
            }
            '`' => {
                lx.next();
                return true;
            }
            _ => lx.next(),
        }
    }
    false
}

/// `#snippet` 的参数：一直吃到下一个配平的右括号之前。
fn snippet_arguments(lx: &mut ScanLexer) -> bool {
    while is_space(lx.lookahead()) {
        lx.skip();
    }
    lx.set_result(SVELTE_RAW_TEXT_SNIPPET_ARGUMENTS as u16);
    let mut level = 0u8;
    let mut advanced = false;
    while !lx.eof() {
        match lx.lookahead() {
            '/' => {
                lx.next();
                if lx.lookahead() == '*' {
                    js_block_comment(lx);
                } else if lx.lookahead() == '/' {
                    js_line_comment(lx);
                }
            }
            '\\' => lx.next(),
            c @ ('"' | '\'') => {
                js_quoted_string(lx, c);
            }
            '`' => {
                js_template_string(lx, 0);
            }
            ')' => {
                if level == 0 {
                    lx.mark_end();
                    return advanced;
                }
                lx.next();
                level -= 1;
            }
            '(' => {
                lx.next();
                level = level.wrapping_add(1);
            }
            _ => lx.next(),
        }
        advanced = true;
    }
    false
}

fn raw_text(lx: &mut ScanLexer, v: &[bool]) -> bool {
    while is_space(lx.lookahead()) {
        lx.skip();
    }
    let c = lx.lookahead();
    if (c == '@' && v[AT]) || (c == '#' && v[HASH]) || (c == ':' && v[COLON]) {
        return false;
    }
    // 带 Svelte 标记符的不是原始文本（比如 `{:else}`）
    if matches!(c, '@' | '#' | ':') {
        return false;
    }
    let mut advanced = false;
    if c == '/' && v[SLASH] {
        lx.next();
        if lx.lookahead() == '*' {
            return js_block_comment(lx);
        }
        if lx.lookahead() != '/' {
            return false;
        }
        advanced = true;
    }
    let each = v[SVELTE_RAW_TEXT_EACH];
    lx.set_result(if each {
        SVELTE_RAW_TEXT_EACH
    } else {
        SVELTE_RAW_TEXT
    } as u16);
    let mut level = 0u8;
    while !lx.eof() {
        match lx.lookahead() {
            '/' => {
                lx.next();
                advanced = true;
                if lx.lookahead() == '*' {
                    js_block_comment(lx);
                } else if lx.lookahead() == '/' {
                    js_line_comment(lx);
                }
            }
            '\\' => {
                lx.next();
                advanced = true;
            }
            c @ ('"' | '\'') => {
                js_quoted_string(lx, c);
                advanced = true;
            }
            '`' => {
                js_template_string(lx, 0);
                advanced = true;
            }
            '}' => {
                if level == 0 {
                    lx.mark_end();
                    return advanced;
                }
                lx.next();
                level -= 1;
                advanced = true;
            }
            '{' => {
                lx.next();
                level = level.wrapping_add(1);
                advanced = true;
            }
            'a' if each => {
                lx.mark_end();
                lx.next();
                advanced = true;
                if lx.lookahead() == 's' {
                    lx.next();
                    if is_space(lx.lookahead()) {
                        return advanced;
                    }
                }
            }
            _ => {
                lx.next();
                advanced = true;
            }
        }
    }
    false
}

/// 在标签扫描之前先试 Svelte 的花括号文本；`None` 表示没有命中，继续走 HTML 的逻辑。
pub(super) fn pre_scan(lx: &mut ScanLexer, v: &[bool]) -> Option<bool> {
    if v[SVELTE_RAW_TEXT_SNIPPET_ARGUMENTS] {
        return Some(snippet_arguments(lx));
    }
    if v[SVELTE_RAW_TEXT] || v[SVELTE_RAW_TEXT_EACH] {
        return Some(raw_text(lx, v));
    }
    None
}
