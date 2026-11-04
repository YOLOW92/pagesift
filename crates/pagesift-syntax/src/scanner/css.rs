//! CSS 外部扫描器，移植自 tree-sitter-css 的 `src/scanner.c`（MIT，Copyright (c) 2018 Max Brunsfeld）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner, is_space};

const DESCENDANT_OP: usize = 0;
const PSEUDO_CLASS_SELECTOR_COLON: usize = 1;
const ERROR_RECOVERY: usize = 2;

#[derive(Default)]
pub struct Css;

impl Scanner for Css {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if v[ERROR_RECOVERY] {
            return false;
        }
        if is_space(lx.lookahead()) && v[DESCENDANT_OP] {
            lx.set_result(DESCENDANT_OP as u16);
            lx.skip();
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            lx.mark_end();
            let c = lx.lookahead();
            if matches!(c, '#' | '.' | '[' | '-' | '*') || c.is_alphanumeric() {
                return true;
            }
            if c == ':' {
                lx.next();
                if is_space(lx.lookahead()) {
                    return false;
                }
                loop {
                    if matches!(lx.lookahead(), ';' | '}') || lx.eof() {
                        return false;
                    }
                    if lx.lookahead() == '{' {
                        return true;
                    }
                    lx.next();
                }
            }
        }
        if v[PSEUDO_CLASS_SELECTOR_COLON] {
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            if lx.lookahead() == ':' {
                lx.next();
                if lx.lookahead() == ':' {
                    return false;
                }
                lx.mark_end();
                lx.set_result(PSEUDO_CLASS_SELECTOR_COLON as u16);
                // 需要 `{` 才是伪类选择器，`;` 说明是属性；注释里的字符不算
                let mut in_comment = false;
                while !matches!(lx.lookahead(), ';' | '}') && !lx.eof() {
                    lx.next();
                    if lx.lookahead() == '{' && !in_comment {
                        return true;
                    }
                    if lx.lookahead() == '/' && !in_comment {
                        lx.next();
                        if lx.lookahead() == '*' {
                            in_comment = true;
                        }
                    } else if lx.lookahead() == '*' && in_comment {
                        lx.next();
                        if lx.lookahead() == '/' {
                            in_comment = false;
                        }
                    }
                }
                // 到了输入末尾也返回伪类选择器，错误恢复时更好
                return lx.eof();
            }
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
