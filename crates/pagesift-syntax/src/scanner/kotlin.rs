//! Kotlin 外部扫描器，移植自 tree-sitter-kotlin 的 `src/scanner.c`（MIT，Copyright (c) 2019 fwcd）。
//! 序列化格式与官方一致（含“待重放注释”的扩展格式）；探测函数拆在 `kotlin/probe.rs`。

mod asi;
mod probe;

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_alpha, is_space};

pub(super) const AUTOMATIC_SEMICOLON: usize = 0;
pub(super) const MULTILINE_COMMENT: usize = 1;
pub(super) const STRING_START: usize = 2;
pub(super) const STRING_END: usize = 3;
pub(super) const STRING_CONTENT: usize = 4;
pub(super) const PRIMARY_CONSTRUCTOR_KEYWORD: usize = 5;
pub(super) const IMPORT_DOT: usize = 6;
pub(super) const INTERPOLATION_EXPRESSION_START: usize = 7;
pub(super) const INTERPOLATION_IDENTIFIER_START: usize = 8;
pub(super) const BY_DELEGATION_HINT: usize = 9;
pub(super) const PRIMARY_CONSTRUCTOR_HINT: usize = 10;
pub(super) const CALLABLE_REFERENCE_QUEST: usize = 11;
pub(super) const COMMENT_CONTINUATION_HINT: usize = 12;
pub(super) const TRY_CLAUSE_HINT: usize = 13;
pub(super) const BACKING_FIELD_HINT: usize = 14;

const DELIMITER_MASK: u8 = 0x3F;
const PREFIX_HI_SHIFT: u32 = 6;
const PREFIX_LEN_MAX: u32 = 1023;
const PREFIX_LEN_UNREPRESENTABLE: u16 = 0;
const PENDING_HEADER_BYTES: usize = 11;
const PENDING_FRAME_BITS: usize = 11;
const MAX_STRING_FRAMES: usize = MAX_SERIALIZED / 2;

#[derive(Clone, Copy, Default)]
pub(super) struct PendingComment {
    pub pending: bool,
    pub start: u32,
    pub end: u32,
    pub semi_owed: bool,
}

#[derive(Default)]
pub struct Kotlin {
    /// 每个打开的字符串两个字节：[分隔符与前缀高位, 前缀低位]。
    strings: Vec<u8>,
    pub(super) constructor_newline: bool,
    pub(super) comment: PendingComment,
}

fn stack_push(stack: &mut Vec<u8>, chr: u8, triple: bool, prefix_len: u16) {
    let delim = if triple { chr + 1 } else { chr };
    let hi = ((prefix_len >> 8) & 0x3) as u8;
    stack.push(delim | (hi << PREFIX_HI_SHIFT));
    stack.push((prefix_len & 0xFF) as u8);
}

fn stack_pop(stack: &mut Vec<u8>) -> bool {
    if stack.len() < 2 {
        return false;
    }
    stack.truncate(stack.len() - 2);
    true
}

fn utf8_width(c: char) -> u32 {
    let c = c as u32;
    if c < 0x80 {
        1
    } else if c < 0x800 {
        2
    } else if c < 0x10000 {
        3
    } else {
        4
    }
}

/// 前进一个字符并累计它的 UTF-8 字节数（官方允许传空指针，这里传一个丢弃用的计数器）。
pub(super) fn advance_counted(lx: &mut ScanLexer, bytes: &mut u32, whitespace: bool) {
    *bytes = bytes.saturating_add(utf8_width(lx.lookahead()));
    lx.advance(whitespace);
}

fn scan_string_start(lx: &mut ScanLexer, stack: &mut Vec<u8>) -> bool {
    if stack.len() + 2 > MAX_SERIALIZED {
        return false;
    }
    let mut dollars = 0u32;
    while lx.lookahead() == '$' {
        lx.next();
        if dollars <= PREFIX_LEN_MAX {
            dollars += 1;
        }
    }
    let prefix_len = if dollars == 0 {
        1
    } else if dollars > PREFIX_LEN_MAX {
        PREFIX_LEN_UNREPRESENTABLE
    } else {
        dollars as u16
    };
    if lx.lookahead() != '"' {
        return false;
    }
    lx.next();
    lx.mark_end();
    for _ in 1..3 {
        if lx.lookahead() != '"' {
            stack_push(stack, b'"', false, prefix_len);
            return true;
        }
        lx.next();
    }
    lx.mark_end();
    stack_push(stack, b'"', true, prefix_len);
    true
}

fn scan_string_content(lx: &mut ScanLexer, stack: &mut Vec<u8>, v: &[bool]) -> bool {
    if stack.len() < 2 {
        return false;
    }
    let packed = stack[stack.len() - 2];
    let prefix_len = (((packed >> PREFIX_HI_SHIFT) as u16) << 8) | stack[stack.len() - 1] as u16;
    let trigger_len = if prefix_len == PREFIX_LEN_UNREPRESENTABLE {
        PREFIX_LEN_MAX + 1
    } else {
        prefix_len as u32
    };
    let raw = packed & DELIMITER_MASK;
    let is_triple = raw & 1 != 0;
    let end_char = (if is_triple { raw - 1 } else { raw }) as char;
    let mut has_content = false;
    loop {
        if lx.lookahead() == '\0' {
            if lx.eof() {
                break;
            }
            lx.next();
            has_content = true;
            continue;
        }
        if lx.lookahead() == '$' {
            if has_content {
                lx.set_result(STRING_CONTENT as u16);
                return true;
            }
            lx.next();
            lx.mark_end();
            let mut additional = 0u32;
            while lx.lookahead() == '$' {
                lx.next();
                if additional <= PREFIX_LEN_MAX {
                    additional += 1;
                }
            }
            let total = 1 + additional;
            let c = lx.lookahead();
            if total >= trigger_len && (is_alpha(c) || c == '_' || c == '{') {
                if total > trigger_len {
                    lx.set_result(STRING_CONTENT as u16);
                    return true;
                }
                if additional > 0 {
                    lx.mark_end();
                }
                if v[INTERPOLATION_EXPRESSION_START] && c == '{' {
                    lx.next();
                    if lx.lookahead() == '}' {
                        return false;
                    }
                    lx.mark_end();
                    lx.set_result(INTERPOLATION_EXPRESSION_START as u16);
                    return true;
                }
                if v[INTERPOLATION_IDENTIFIER_START] && (is_alpha(c) || c == '_') {
                    lx.set_result(INTERPOLATION_IDENTIFIER_START as u16);
                    return true;
                }
                return false;
            }
            if additional > 0 {
                lx.mark_end();
            }
            lx.set_result(STRING_CONTENT as u16);
            return true;
        }
        if lx.lookahead() == '\\' {
            lx.next();
            if lx.lookahead() == '$' {
                lx.next();
                if lx.lookahead() == end_char {
                    if !stack_pop(stack) {
                        return false;
                    }
                    lx.next();
                    lx.mark_end();
                    lx.set_result(STRING_END as u16);
                    return true;
                }
            } else if is_triple && lx.lookahead() == end_char {
                has_content = true;
                continue;
            }
        } else if lx.lookahead() == end_char {
            if is_triple {
                lx.mark_end();
                for _ in 1..3 {
                    lx.next();
                    if lx.lookahead() != end_char {
                        lx.mark_end();
                        lx.set_result(STRING_CONTENT as u16);
                        return true;
                    }
                }
                if has_content && lx.lookahead() == end_char {
                    lx.set_result(STRING_CONTENT as u16);
                    return true;
                }
                lx.set_result(STRING_END as u16);
                lx.mark_end();
                while lx.lookahead() == end_char {
                    lx.next();
                    lx.mark_end();
                }
                return stack_pop(stack);
            }
            if has_content {
                lx.mark_end();
                lx.set_result(STRING_CONTENT as u16);
                return true;
            }
            if !stack_pop(stack) {
                return false;
            }
            lx.next();
            lx.mark_end();
            lx.set_result(STRING_END as u16);
            return true;
        } else {
            lx.next();
            has_content = true;
            continue;
        }
        lx.next();
        has_content = true;
    }
    false
}

/// `newline` 和 `bytes` 官方都可传空；这里调用方传丢弃用的变量。
pub(super) fn scan_multiline_comment(
    lx: &mut ScanLexer,
    newline: &mut bool,
    bytes: &mut u32,
) -> bool {
    if lx.lookahead() != '/' {
        return false;
    }
    advance_counted(lx, bytes, false);
    if lx.lookahead() != '*' {
        return false;
    }
    advance_counted(lx, bytes, false);
    let mut after_star = false;
    let mut depth = 1u32;
    loop {
        if matches!(lx.lookahead(), '\n' | '\r') {
            *newline = true;
        }
        match lx.lookahead() {
            '*' => {
                advance_counted(lx, bytes, false);
                after_star = true;
            }
            '/' => {
                advance_counted(lx, bytes, false);
                if after_star {
                    after_star = false;
                    depth -= 1;
                    if depth == 0 {
                        lx.set_result(MULTILINE_COMMENT as u16);
                        lx.mark_end();
                        return true;
                    }
                } else {
                    after_star = false;
                    if lx.lookahead() == '*' {
                        depth += 1;
                        advance_counted(lx, bytes, false);
                    }
                }
            }
            '\0' if lx.eof() => {
                lx.set_result(MULTILINE_COMMENT as u16);
                lx.mark_end();
                return true;
            }
            _ => {
                advance_counted(lx, bytes, false);
                after_star = false;
            }
        }
    }
}

fn write_u32(buf: &mut [u8], v: u32) {
    buf[..4].copy_from_slice(&v.to_le_bytes());
}

fn read_u32(buf: &[u8]) -> u32 {
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

impl Scanner for Kotlin {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        self.run(lx, v)
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        let n = self.strings.len();
        if n > MAX_SERIALIZED || n % 2 == 1 {
            return;
        }
        if self.comment.pending {
            let frames = n / 2;
            let length = PENDING_HEADER_BYTES + (frames * PENDING_FRAME_BITS).div_ceil(8);
            let mut buf = vec![0u8; length];
            buf[0] = 8
                | if self.constructor_newline { 4 } else { 0 }
                | if self.comment.semi_owed { 16 } else { 0 };
            buf[1] = frames as u8;
            buf[2] = (frames >> 8) as u8;
            write_u32(&mut buf[3..], self.comment.start);
            write_u32(&mut buf[7..], self.comment.end);
            for i in 0..frames {
                let d = self.strings[2 * i];
                let value = (((d >> PREFIX_HI_SHIFT) as usize) << 8)
                    | self.strings[2 * i + 1] as usize
                    | (((d & 1) as usize) << 10);
                for bit in 0..PENDING_FRAME_BITS {
                    let pos = i * PENDING_FRAME_BITS + bit;
                    if value & (1 << bit) != 0 {
                        buf[PENDING_HEADER_BYTES + pos / 8] |= 1 << (pos % 8);
                    }
                }
            }
            out.extend_from_slice(&buf);
            return;
        }
        if n > 0 {
            out.extend_from_slice(&self.strings);
            if self.constructor_newline {
                out[0] |= 4;
            }
        } else if self.constructor_newline {
            out.push(4);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.strings.clear();
        self.constructor_newline = false;
        self.comment = PendingComment::default();
        if d.is_empty() || d.len() > MAX_SERIALIZED {
            return;
        }
        let flags = d[0];
        if flags & 3 == 0 && flags & 8 != 0 {
            if d.len() < PENDING_HEADER_BYTES || flags & !28 != 0 {
                return;
            }
            let frames = d[1] as usize | ((d[2] as usize) << 8);
            if frames > MAX_STRING_FRAMES
                || d.len() != PENDING_HEADER_BYTES + (frames * PENDING_FRAME_BITS).div_ceil(8)
            {
                return;
            }
            let (start, end) = (read_u32(&d[3..]), read_u32(&d[7..]));
            if end < start || end - start < 4 {
                return;
            }
            for i in 0..frames {
                let mut value = 0usize;
                for bit in 0..PENDING_FRAME_BITS {
                    let pos = i * PENDING_FRAME_BITS + bit;
                    if d[PENDING_HEADER_BYTES + pos / 8] & (1 << (pos % 8)) != 0 {
                        value |= 1 << bit;
                    }
                }
                stack_push(
                    &mut self.strings,
                    b'"',
                    value & 1024 != 0,
                    (value & 1023) as u16,
                );
            }
            self.constructor_newline = flags & 4 != 0;
            self.comment = PendingComment {
                pending: true,
                start,
                end,
                semi_owed: flags & 16 != 0,
            };
            return;
        }
        if d.len() == 1 && flags == 4 {
            self.constructor_newline = true;
            return;
        }
        if d.len() % 2 == 1 {
            return;
        }
        self.strings.extend_from_slice(d);
        self.strings[0] &= !4;
        self.constructor_newline = flags & 4 != 0;
    }
}

impl Kotlin {
    fn run(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if self.comment.pending {
            return self.resume_pending_comment(lx, v);
        }
        if v[PRIMARY_CONSTRUCTOR_HINT] && !v[STRING_CONTENT] {
            return self.scan_primary_constructor_hint(lx, v);
        }
        self.constructor_newline = false;
        if v[AUTOMATIC_SEMICOLON] {
            let mut clean = false;
            let ret = self.scan_automatic_semicolon(lx, v, &mut clean);
            if ret {
                return ret;
            }
            if !clean && !v[STRING_CONTENT] {
                return false;
            }
        }
        if v[CALLABLE_REFERENCE_QUEST] && lx.lookahead() == '?' {
            lx.next();
            lx.mark_end();
            while matches!(lx.lookahead(), ' ' | '\t') {
                lx.skip();
            }
            if lx.lookahead() == '?' {
                lx.set_result(CALLABLE_REFERENCE_QUEST as u16);
                return true;
            }
            if lx.lookahead() == ':' {
                lx.next();
                if lx.lookahead() == ':' {
                    lx.set_result(CALLABLE_REFERENCE_QUEST as u16);
                    return true;
                }
            }
            return false;
        }
        if v[IMPORT_DOT] && probe::scan_import_dot(lx) {
            return true;
        }
        if v[PRIMARY_CONSTRUCTOR_KEYWORD] && !v[STRING_CONTENT] {
            while is_space(lx.lookahead()) {
                lx.skip();
            }
            if lx.lookahead() == 'c' && probe::eat_constructor(lx) {
                lx.set_result(PRIMARY_CONSTRUCTOR_KEYWORD as u16);
                lx.mark_end();
                return true;
            }
        }
        if (v[STRING_CONTENT]
            || v[INTERPOLATION_EXPRESSION_START]
            || v[INTERPOLATION_IDENTIFIER_START])
            && scan_string_content(lx, &mut self.strings, v)
        {
            return true;
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if v[STRING_START] && scan_string_start(lx, &mut self.strings) {
            lx.set_result(STRING_START as u16);
            return true;
        }
        if v[MULTILINE_COMMENT] && scan_multiline_comment(lx, &mut false, &mut 0) {
            return true;
        }
        false
    }
}
