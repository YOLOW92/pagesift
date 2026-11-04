//! Elixir 外部扫描器，移植自 tree-sitter-elixir 的 `src/scanner.c`（Apache-2.0）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner};

const QUOTED_CONTENT_I_SINGLE: usize = 0;
const QUOTED_CONTENT_I_DOUBLE: usize = 1;
const NEWLINE_BEFORE_DO: usize = 20;
const NEWLINE_BEFORE_BINARY_OPERATOR: usize = 21;
const NEWLINE_BEFORE_COMMENT: usize = 22;
const BEFORE_UNARY_OPERATOR: usize = 23;
const NOT_IN: usize = 24;
const QUOTED_ATOM_START: usize = 25;

/// （记号, 是否支持插值, 结束符, 结束符重复次数），顺序与官方枚举一致。
const QUOTED: [(bool, char, u8); 20] = [
    (true, '\'', 1),
    (true, '"', 1),
    (true, '\'', 3),
    (true, '"', 3),
    (true, ')', 1),
    (true, '}', 1),
    (true, ']', 1),
    (true, '>', 1),
    (true, '|', 1),
    (true, '/', 1),
    (false, '\'', 1),
    (false, '"', 1),
    (false, '\'', 3),
    (false, '"', 3),
    (false, ')', 1),
    (false, '}', 1),
    (false, ']', 1),
    (false, '>', 1),
    (false, '|', 1),
    (false, '/', 1),
];

#[derive(Default)]
pub struct Elixir;

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn is_inline_ws(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r')
}

fn check_keyword_end(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() == ':' {
        lx.next();
        return is_ws(lx.lookahead());
    }
    false
}

fn check_operator_end(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() == ':' {
        return !check_keyword_end(lx);
    }
    while is_inline_ws(lx.lookahead()) {
        lx.next();
    }
    if lx.lookahead() == '/' {
        lx.next();
        while is_ws(lx.lookahead()) {
            lx.next();
        }
        if lx.lookahead().is_ascii_digit() {
            return false;
        }
    }
    true
}

fn is_token_end(c: char) -> bool {
    "@.+-^*/<>|~=&\\%{}[]()\"',;#".contains(c) || is_ws(c)
}

fn scan_quoted_content(lx: &mut ScanLexer, idx: usize) -> bool {
    let (interp, end, len) = QUOTED[idx];
    lx.set_result(idx as u16);
    let heredoc = len == 3;
    let mut has_content = false;
    loop {
        let mut newline = false;
        if is_newline(lx.lookahead()) {
            lx.next();
            has_content = true;
            newline = true;
            while is_ws(lx.lookahead()) {
                lx.next();
            }
        }
        lx.mark_end();
        if lx.lookahead() == end {
            let mut length = 1;
            while length < len {
                lx.next();
                if lx.lookahead() == end {
                    length += 1;
                } else {
                    break;
                }
            }
            if length == len && (!heredoc || newline) {
                return has_content;
            }
        } else if lx.lookahead() == '#' {
            lx.next();
            if interp && lx.lookahead() == '{' {
                return has_content;
            }
        } else if lx.lookahead() == '\\' {
            lx.next();
            if heredoc && lx.lookahead() == '\n' {
                // 要靠换行认出 heredoc 的结束符，所以有意不处理这里的转义
            } else if interp || lx.lookahead() == end {
                return has_content;
            }
        } else if lx.lookahead() == '\0' {
            // 到了文件末尾还没有结束符：语法非法，已扫描的内容都算引号内容
            return has_content;
        } else {
            lx.next();
        }
        has_content = true;
    }
}

/// 连续吃掉 `chars` 里的每个字符（逐个 `next`），遇到不符返回假。
fn eat_all(lx: &mut ScanLexer, chars: &str) -> bool {
    for c in chars.chars() {
        if lx.lookahead() != c {
            return false;
        }
        lx.next();
    }
    true
}

/// 换行之后的二元运算符；写法与官方的 if 链一一对应，没有命中的分支都落到最后的“假”。
fn scan_binary_operator(lx: &mut ScanLexer) -> bool {
    let c = lx.lookahead();
    match c {
        '&' => {
            lx.next();
            if lx.lookahead() == '&' {
                lx.next();
                if lx.lookahead() == '&' {
                    lx.next();
                }
                return check_operator_end(lx);
            }
        }
        '=' => {
            lx.next();
            if lx.lookahead() == '=' {
                lx.next();
                if lx.lookahead() == '=' {
                    lx.next();
                }
            } else if matches!(lx.lookahead(), '~' | '>') {
                lx.next();
            }
            return check_operator_end(lx);
        }
        ':' => {
            lx.next();
            if lx.lookahead() == ':' {
                lx.next();
                if lx.lookahead() == ':' {
                    return false;
                }
                return check_operator_end(lx);
            }
        }
        '+' => {
            lx.next();
            if lx.lookahead() == '+' {
                lx.next();
                if lx.lookahead() == '+' {
                    lx.next();
                }
                return check_operator_end(lx);
            }
        }
        '-' => {
            lx.next();
            if lx.lookahead() == '-' {
                lx.next();
                if lx.lookahead() == '-' {
                    lx.next();
                }
                return check_operator_end(lx);
            } else if lx.lookahead() == '>' {
                lx.next();
                return check_operator_end(lx);
            }
        }
        '<' => {
            lx.next();
            match lx.lookahead() {
                '=' | '-' | '>' => {
                    lx.next();
                    return check_operator_end(lx);
                }
                '~' => {
                    lx.next();
                    if lx.lookahead() == '>' {
                        lx.next();
                    }
                    return check_operator_end(lx);
                }
                '|' => {
                    lx.next();
                    if lx.lookahead() == '>' {
                        lx.next();
                        return check_operator_end(lx);
                    }
                }
                '<' => {
                    lx.next();
                    if matches!(lx.lookahead(), '<' | '~') {
                        lx.next();
                        return check_operator_end(lx);
                    }
                }
                _ => return check_operator_end(lx),
            }
        }
        '>' => {
            lx.next();
            if lx.lookahead() == '=' {
                lx.next();
                return check_operator_end(lx);
            } else if lx.lookahead() == '>' {
                lx.next();
                if lx.lookahead() == '>' {
                    lx.next();
                    return check_operator_end(lx);
                }
            } else {
                return check_operator_end(lx);
            }
        }
        '^' if eat_all(lx, "^^^") => {
            return check_operator_end(lx);
        }
        '!' => {
            lx.next();
            if lx.lookahead() == '=' {
                lx.next();
                if lx.lookahead() == '=' {
                    lx.next();
                }
                return check_operator_end(lx);
            }
        }
        '~' => {
            lx.next();
            if lx.lookahead() == '>' {
                lx.next();
                if lx.lookahead() == '>' {
                    lx.next();
                }
                return check_operator_end(lx);
            }
        }
        '|' => {
            lx.next();
            if lx.lookahead() == '|' {
                lx.next();
                if lx.lookahead() == '|' {
                    lx.next();
                }
            } else if lx.lookahead() == '>' {
                lx.next();
            }
            return check_operator_end(lx);
        }
        '*' => {
            lx.next();
            if lx.lookahead() == '*' {
                lx.next();
            }
            return check_operator_end(lx);
        }
        '/' => {
            lx.next();
            if lx.lookahead() == '/' {
                lx.next();
            }
            return check_operator_end(lx);
        }
        '.' => {
            lx.next();
            if lx.lookahead() == '.' {
                lx.next();
                if lx.lookahead() == '.' {
                    return false;
                }
            }
            return check_operator_end(lx);
        }
        '\\' => {
            lx.next();
            if lx.lookahead() == '\\' {
                lx.next();
                return check_operator_end(lx);
            }
        }
        'w' if eat_all(lx, "when") => {
            return is_token_end(lx.lookahead()) && check_operator_end(lx);
        }
        'a' if eat_all(lx, "and") => {
            return is_token_end(lx.lookahead()) && check_operator_end(lx);
        }
        'o' if eat_all(lx, "or") => {
            return is_token_end(lx.lookahead()) && check_operator_end(lx);
        }
        'i' if eat_all(lx, "in") => {
            return is_token_end(lx.lookahead()) && check_operator_end(lx);
        }
        'n' if eat_all(lx, "not") => {
            while is_inline_ws(lx.lookahead()) {
                lx.next();
            }
            if eat_all(lx, "in") {
                return is_token_end(lx.lookahead()) && check_operator_end(lx);
            }
        }
        _ => {}
    }
    false
}

fn scan_newline(lx: &mut ScanLexer, v: &[bool]) -> bool {
    lx.next();
    while is_ws(lx.lookahead()) {
        lx.next();
    }
    // 把换行后的空白一起吞掉，解析器就不用再走一遍
    lx.mark_end();
    if lx.lookahead() == '#' {
        lx.set_result(NEWLINE_BEFORE_COMMENT as u16);
        return true;
    }
    if lx.lookahead() == 'd' && v[NEWLINE_BEFORE_DO] {
        lx.set_result(NEWLINE_BEFORE_DO as u16);
        lx.next();
        if lx.lookahead() == 'o' {
            lx.next();
            return is_token_end(lx.lookahead());
        }
        return false;
    }
    if v[NEWLINE_BEFORE_BINARY_OPERATOR] {
        lx.set_result(NEWLINE_BEFORE_BINARY_OPERATOR as u16);
        return scan_binary_operator(lx);
    }
    false
}

impl Scanner for Elixir {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        // 引号内容的记号互斥；多个同时有效说明在解析引号外的任意代码
        if !(v[QUOTED_CONTENT_I_SINGLE] && v[QUOTED_CONTENT_I_DOUBLE])
            && let Some(idx) = (0..QUOTED.len()).find(|&i| v[i])
        {
            return scan_quoted_content(lx, idx);
        }
        let mut skipped = false;
        while is_inline_ws(lx.lookahead()) {
            skipped = true;
            lx.skip();
        }
        if is_newline(lx.lookahead())
            && (v[NEWLINE_BEFORE_DO]
                || v[NEWLINE_BEFORE_BINARY_OPERATOR]
                || v[NEWLINE_BEFORE_COMMENT])
        {
            return scan_newline(lx, v);
        }
        match lx.lookahead() {
            '+' if skipped && v[BEFORE_UNARY_OPERATOR] => {
                lx.mark_end();
                lx.next();
                if matches!(lx.lookahead(), '+' | ':' | '/') || is_ws(lx.lookahead()) {
                    return false;
                }
                lx.set_result(BEFORE_UNARY_OPERATOR as u16);
                return true;
            }
            '-' if skipped && v[BEFORE_UNARY_OPERATOR] => {
                lx.mark_end();
                lx.set_result(BEFORE_UNARY_OPERATOR as u16);
                lx.next();
                if matches!(lx.lookahead(), '-' | '>' | ':' | '/') || is_ws(lx.lookahead()) {
                    return false;
                }
                return true;
            }
            'n' if v[NOT_IN] => {
                lx.set_result(NOT_IN as u16);
                if eat_all(lx, "not") {
                    while is_inline_ws(lx.lookahead()) {
                        lx.next();
                    }
                    if eat_all(lx, "in") {
                        return is_token_end(lx.lookahead());
                    }
                }
            }
            ':' if v[QUOTED_ATOM_START] => {
                lx.next();
                lx.mark_end();
                lx.set_result(QUOTED_ATOM_START as u16);
                if matches!(lx.lookahead(), '"' | '\'') {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
