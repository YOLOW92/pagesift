//! Vue 外部扫描器的文本片段部分，移植自 tree-sitter-vue 的 `src/scanner.c`
//! （MIT，Copyright (c) 2019 ikatyang 等）。标签栈部分与 HTML 共用 `html.rs`（`Html::vue()`）。

use super::{ScanLexer, is_space};

pub(super) const TEMPLATE_START_TAG_NAME: usize = 9;
const TEXT_FRAGMENT: usize = 10;
const INTERPOLATION_TEXT: usize = 11;
const START_TAG_NAME: usize = 0;
const RAW_TEXT: usize = 7;
const COMMENT: usize = 8;

/// 在标签扫描之前先试文本片段和插值文本；`None` 表示没有命中，继续走 HTML 的逻辑。
pub(super) fn pre_scan(lx: &mut ScanLexer, v: &[bool]) -> Option<bool> {
    let error_recovery = v[START_TAG_NAME] && v[RAW_TEXT];
    if error_recovery || lx.lookahead() == '<' || !(v[TEXT_FRAGMENT] || v[INTERPOLATION_TEXT]) {
        return None;
    }
    let mut advanced = false;
    if !v[COMMENT] {
        while is_space(lx.lookahead()) {
            lx.skip();
        }
    }
    let mut exited = false;
    while !lx.eof() {
        match lx.lookahead() {
            '<' => {
                lx.mark_end();
                lx.next();
                if lx.lookahead().is_alphabetic() || matches!(lx.lookahead(), '!' | '?' | '/') {
                    exited = true;
                    break;
                }
                advanced = true;
            }
            '{' => {
                lx.mark_end();
                lx.next();
                if lx.lookahead() == '{' {
                    exited = true;
                    break;
                }
                advanced = true;
            }
            '}' => {
                if v[INTERPOLATION_TEXT] {
                    lx.mark_end();
                    lx.next();
                    if lx.lookahead() == '}' {
                        lx.set_result(INTERPOLATION_TEXT as u16);
                        return Some(advanced);
                    }
                } else {
                    lx.next();
                    advanced = true;
                }
            }
            c @ ('\r' | '\n') => {
                let mut newline = c == '\n';
                if c == '\r' {
                    lx.next();
                    // 不是 CRLF 就按普通字符处理，否则落到换行分支
                    if lx.lookahead() != '\n' {
                        advanced = true;
                        lx.next();
                        continue;
                    }
                    newline = true;
                }
                if newline && v[TEXT_FRAGMENT] {
                    lx.mark_end();
                    while is_space(lx.lookahead()) {
                        if advanced {
                            lx.next();
                        } else {
                            lx.skip();
                        }
                    }
                    if matches!(lx.lookahead(), '<' | '>') {
                        exited = true;
                        break;
                    }
                } else {
                    lx.next();
                }
            }
            c => {
                advanced |= c != '\n';
                lx.next();
            }
        }
    }
    if !exited && lx.eof() {
        return Some(false);
    }
    if advanced {
        lx.set_result(TEXT_FRAGMENT as u16);
        return Some(true);
    }
    None
}
