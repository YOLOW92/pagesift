//! Elm 外部扫描器，移植自 tree-sitter-elm 的 `src/scanner.c`（MIT）。
//! 缩进栈和“回退”队列的元素都是单字节；序列化格式与官方一致。

use super::{ScanLexer, Scanner};

const VIRTUAL_END_DECL: usize = 0;
const VIRTUAL_OPEN_SECTION: usize = 1;
const VIRTUAL_END_SECTION: usize = 2;
const MINUS_WITHOUT_TRAILING_WHITESPACE: usize = 3;
const GLSL_CONTENT: usize = 4;
const BLOCK_COMMENT_CONTENT: usize = 5;
const STRING_CONTENT_MULTILINE: usize = 6;

const MAX_INDENT_DEPTH: usize = 256;
/// 嵌套块注释的递归深度上限（输入不可信）。
const MAX_COMMENT_NEST: u32 = 256;
const MAX_SERIALIZED: usize = super::MAX_SERIALIZED;

#[derive(Default)]
pub struct Elm {
    indent_length: u32,
    indents: Vec<u8>,
    runback: Vec<u8>,
}

fn in_error_recovery(v: &[bool]) -> bool {
    (0..=6).all(|i| v[i])
}

fn is_elm_space(lx: &ScanLexer) -> bool {
    matches!(lx.lookahead(), ' ' | '\r' | '\n')
}

/// 0：没有；1：只读到一部分；2：读到完整的 `in`。
fn check_for_in(lx: &mut ScanLexer, v: &[bool]) -> u8 {
    if v[VIRTUAL_END_SECTION] && lx.lookahead() == 'i' {
        lx.skip();
        if lx.lookahead() == 'n' {
            lx.skip();
            if is_elm_space(lx) || lx.eof() {
                return 2;
            }
            return 1;
        }
        return 1;
    }
    0
}

fn scan_block_comment(lx: &mut ScanLexer, nest: u32) -> bool {
    lx.mark_end();
    if lx.lookahead() != '{' {
        return false;
    }
    lx.next();
    if lx.lookahead() != '-' {
        return false;
    }
    lx.next();
    loop {
        match lx.lookahead() {
            '{' => {
                if nest < MAX_COMMENT_NEST {
                    scan_block_comment(lx, nest + 1);
                } else {
                    return true;
                }
            }
            '-' => {
                lx.next();
                if lx.lookahead() == '}' {
                    lx.next();
                    return true;
                }
            }
            '\0' => return true,
            _ => lx.next(),
        }
    }
}

fn advance_to_line_end(lx: &mut ScanLexer) {
    while lx.lookahead() != '\n' && !lx.eof() {
        lx.next();
    }
}

/// 跳过一段（可嵌套的）块注释体，调用时已经读过开头的 `{-`。
fn skip_comment_body(lx: &mut ScanLexer) {
    let mut nesting = 1;
    while nesting > 0 && !lx.eof() {
        if lx.lookahead() == '{' {
            lx.next();
            if lx.lookahead() == '-' {
                lx.next();
                nesting += 1;
            }
        } else if lx.lookahead() == '-' {
            lx.next();
            if lx.lookahead() == '}' {
                lx.next();
                nesting -= 1;
            }
        } else {
            lx.next();
        }
    }
}

impl Elm {
    fn back(&self) -> Option<u8> {
        self.indents.last().copied()
    }

    /// 跳过注释后的空白并重新量缩进。
    fn remeasure(&mut self, lx: &mut ScanLexer) {
        while matches!(lx.lookahead(), ' ' | '\n' | '\r' | '\t') {
            if lx.lookahead() == '\n' {
                lx.next();
                while lx.lookahead() == ' ' {
                    lx.next();
                }
                self.indent_length = lx.column();
            } else {
                lx.next();
            }
        }
    }

    fn run(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if in_error_recovery(v) {
            return false;
        }
        // 先处理上一次扫描留下的回退记号
        if self.runback.last() == Some(&0) && v[VIRTUAL_END_DECL] {
            self.runback.pop();
            lx.set_result(VIRTUAL_END_DECL as u16);
            return true;
        }
        if self.runback.last() == Some(&1) && v[VIRTUAL_END_SECTION] {
            self.runback.pop();
            lx.set_result(VIRTUAL_END_SECTION as u16);
            return true;
        }
        self.runback.clear();

        // 多行字符串内容要在空白和注释处理之前
        if v[STRING_CONTENT_MULTILINE] {
            lx.set_result(STRING_CONTENT_MULTILINE as u16);
            let mut has_content = false;
            loop {
                match lx.lookahead() {
                    '"' => {
                        lx.mark_end();
                        lx.next();
                        if lx.lookahead() == '"' {
                            lx.next();
                            if lx.lookahead() == '"' {
                                return has_content;
                            }
                        }
                        has_content = true;
                    }
                    '\\' | '\0' => {
                        lx.mark_end();
                        return has_content;
                    }
                    _ => {
                        has_content = true;
                        lx.next();
                    }
                }
            }
        }

        let mut has_newline = false;
        let mut found_in = false;
        let mut can_mark_end = true;
        lx.mark_end();
        loop {
            let c = lx.lookahead();
            if c == ' ' || c == '\r' {
                lx.skip();
            } else if c == '\n' {
                lx.skip();
                has_newline = true;
                loop {
                    if lx.lookahead() == ' ' {
                        lx.skip();
                    } else {
                        self.indent_length = lx.column();
                        break;
                    }
                }
            } else if !v[BLOCK_COMMENT_CONTENT] && c == '-' {
                lx.next();
                let la = lx.lookahead();
                if v[MINUS_WITHOUT_TRAILING_WHITESPACE]
                    && (la.is_ascii_alphabetic() || la == '(' || la as u32 > 127)
                {
                    if can_mark_end {
                        lx.set_result(MINUS_WITHOUT_TRAILING_WHITESPACE as u16);
                        lx.mark_end();
                        return true;
                    }
                    return false;
                }
                // 行注释对这里要找的记号来说等同空白，但之后不能再 mark_end，否则注释会丢
                if la == '-' && has_newline {
                    can_mark_end = false;
                    lx.next();
                    advance_to_line_end(lx);
                } else if v[BLOCK_COMMENT_CONTENT] && lx.lookahead() == '}' {
                    lx.set_result(BLOCK_COMMENT_CONTENT as u16);
                    return true;
                } else {
                    return false;
                }
            } else if lx.eof() {
                if v[VIRTUAL_END_SECTION] {
                    lx.set_result(VIRTUAL_END_SECTION as u16);
                    return true;
                }
                if v[VIRTUAL_END_DECL] {
                    lx.set_result(VIRTUAL_END_DECL as u16);
                    return true;
                }
                break;
            } else {
                break;
            }
        }

        if check_for_in(lx, v) == 2 {
            if has_newline {
                found_in = true;
            } else {
                lx.set_result(VIRTUAL_END_SECTION as u16);
                self.indents.pop();
                return true;
            }
        }

        // 右括号、逗号、右花括号之前要先结束打开的 case/let 段
        if v[VIRTUAL_END_SECTION] && matches!(lx.lookahead(), ')' | ',' | '}') {
            lx.set_result(VIRTUAL_END_SECTION as u16);
            self.indents.pop();
            return true;
        }

        if v[VIRTUAL_OPEN_SECTION] && !lx.eof() {
            if self.indents.len() >= MAX_INDENT_DEPTH {
                return false;
            }
            self.indents.push(lx.column() as u8);
            lx.set_result(VIRTUAL_OPEN_SECTION as u16);
            return true;
        }
        if v[BLOCK_COMMENT_CONTENT] {
            if !can_mark_end {
                return false;
            }
            lx.mark_end();
            loop {
                let c = lx.lookahead();
                if c == '\0' {
                    break;
                }
                if c != '{' && c != '-' {
                    lx.next();
                } else if c == '-' {
                    lx.mark_end();
                    lx.next();
                    if lx.lookahead() == '}' {
                        break;
                    }
                } else if scan_block_comment(lx, 0) {
                    lx.mark_end();
                }
            }
            lx.set_result(BLOCK_COMMENT_CONTENT as u16);
            return true;
        }
        if has_newline {
            self.runback.clear();
            // 块注释所在的列不代表真正的缩进：先越过它再量
            if lx.lookahead() == '{'
                && !v[BLOCK_COMMENT_CONTENT]
                && self.back().is_some_and(|b| self.indent_length < b as u32)
            {
                lx.next();
                if lx.lookahead() == '-' {
                    can_mark_end = false;
                    lx.next();
                    skip_comment_body(lx);
                    self.remeasure(lx);
                    while lx.lookahead() == '{' {
                        lx.next();
                        if lx.lookahead() == '-' {
                            lx.next();
                            skip_comment_body(lx);
                            self.remeasure(lx);
                        } else {
                            break;
                        }
                    }
                }
            }
            while let Some(top) = self.back() {
                if self.indent_length > top as u32 {
                    break;
                }
                if self.indent_length == top as u32 {
                    if found_in {
                        self.indents.pop();
                        self.runback.push(1);
                        found_in = false;
                        break;
                    }
                    // 后面紧跟行注释或块注释时不插 VIRTUAL_END_DECL
                    if lx.lookahead() == '-' {
                        lx.skip();
                        if lx.lookahead() == '-' {
                            break;
                        }
                    }
                    if lx.lookahead() == '{' {
                        lx.skip();
                        if lx.lookahead() == '-' {
                            break;
                        }
                    }
                    self.runback.push(0);
                    break;
                }
                // indent_length < top
                self.indents.pop();
                self.runback.push(1);
                if found_in && self.back().is_none_or(|b| self.indent_length > b as u32) {
                    found_in = false;
                }
            }
            if found_in && !self.indents.is_empty() {
                self.indents.pop();
                self.runback.push(1);
            }
            self.runback.reverse();
            if self.runback.last() == Some(&0) && v[VIRTUAL_END_DECL] {
                self.runback.pop();
                lx.set_result(VIRTUAL_END_DECL as u16);
                return true;
            }
            if self.runback.last() == Some(&1) && v[VIRTUAL_END_SECTION] {
                self.runback.pop();
                lx.set_result(VIRTUAL_END_SECTION as u16);
                return true;
            }
            if lx.eof() && v[VIRTUAL_END_SECTION] {
                lx.set_result(VIRTUAL_END_SECTION as u16);
                return true;
            }
        }
        if v[GLSL_CONTENT] {
            if !can_mark_end {
                return false;
            }
            lx.set_result(GLSL_CONTENT as u16);
            loop {
                match lx.lookahead() {
                    '|' => {
                        lx.mark_end();
                        lx.next();
                        if lx.lookahead() == ']' {
                            lx.next();
                            return true;
                        }
                    }
                    '\0' => {
                        lx.mark_end();
                        return true;
                    }
                    _ => lx.next(),
                }
            }
        }
        false
    }
}

impl Scanner for Elm {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        self.run(lx, v)
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        if 3 + self.indents.len() + self.runback.len() >= MAX_SERIALIZED {
            return;
        }
        let rc = self.runback.len().min(255);
        out.push(rc as u8);
        out.extend_from_slice(&self.runback[..rc]);
        out.push(4);
        out.extend_from_slice(&self.indent_length.to_le_bytes());
        for b in self.indents.iter().skip(1) {
            if out.len() >= MAX_SERIALIZED {
                break;
            }
            out.push(*b);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.runback.clear();
        self.indents.clear();
        self.indents.push(0);
        if d.is_empty() {
            return;
        }
        let mut p = 0;
        let rc = d[p] as usize;
        p += 1;
        if rc > 0 {
            let n = rc.min(d.len() - p);
            self.runback.extend_from_slice(&d[p..p + n]);
            p += n;
        }
        if p >= d.len() {
            return;
        }
        let il = d[p] as usize;
        p += 1;
        if il > 0 {
            let n = il.min(4).min(d.len() - p);
            let mut raw = [0u8; 4];
            raw[..n].copy_from_slice(&d[p..p + n]);
            self.indent_length = u32::from_le_bytes(raw);
            p += il.min(d.len() - p);
        }
        self.indents.extend_from_slice(&d[p.min(d.len())..]);
    }
}
