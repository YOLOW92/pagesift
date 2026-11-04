//! Kotlin 扫描器的探测与自动分号分支（拆自 `kotlin.rs`，逻辑不变）。

use super::*;

pub(super) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 已经认出首字母的关键字：先跳过首字母，再逐个比较其余字符，最后确认标识符在此结束。
pub(super) fn scan_for_word(lx: &mut ScanLexer, word: &str) -> bool {
    lx.skip();
    for ch in word.chars() {
        if lx.lookahead() != ch {
            return false;
        }
        lx.skip();
    }
    !is_word_char(lx.lookahead())
}

pub(super) fn check_word(lx: &mut ScanLexer, word: &str) -> bool {
    for ch in word.chars() {
        if lx.lookahead() != ch {
            return false;
        }
        lx.skip();
    }
    !is_word_char(lx.lookahead())
}

/// 用 skip 跳过空白和注释；遇到不是注释的 `/` 返回假。
pub(super) fn skip_ws_comments(lx: &mut ScanLexer) -> bool {
    loop {
        while is_space(lx.lookahead()) {
            lx.skip();
        }
        if lx.lookahead() != '/' {
            return true;
        }
        lx.skip();
        if lx.lookahead() == '/' {
            lx.skip();
            while !matches!(lx.lookahead(), '\n' | '\r') && !lx.eof() {
                lx.skip();
            }
        } else if lx.lookahead() == '*' {
            lx.skip();
            let mut depth = 1;
            while depth > 0 && !lx.eof() {
                if lx.lookahead() == '*' {
                    lx.skip();
                    if lx.lookahead() == '/' {
                        lx.skip();
                        depth -= 1;
                    }
                } else if lx.lookahead() == '/' {
                    lx.skip();
                    if lx.lookahead() == '*' {
                        lx.skip();
                        depth += 1;
                    }
                } else {
                    lx.skip();
                }
            }
        } else {
            return false;
        }
    }
}

pub(super) fn followed_by_arrow(lx: &mut ScanLexer) -> bool {
    if !skip_ws_comments(lx) || lx.lookahead() != '-' {
        return false;
    }
    lx.skip();
    lx.lookahead() == '>'
}

pub(super) fn followed_by(lx: &mut ScanLexer, expected: char) -> bool {
    skip_ws_comments(lx) && lx.lookahead() == expected
}

/// 可见性修饰符，后面（同一行）紧跟 `target`。
pub(super) fn check_modifier_then(lx: &mut ScanLexer, target: &str) -> bool {
    let mut word = String::new();
    while is_word_char(lx.lookahead()) && word.len() < 19 {
        word.push(lx.lookahead());
        lx.skip();
    }
    if !matches!(
        word.as_str(),
        "public" | "private" | "protected" | "internal"
    ) {
        return false;
    }
    while matches!(lx.lookahead(), ' ' | '\t') {
        lx.skip();
    }
    check_word(lx, target)
}

/// 跳过若干注解（含 `@a.b.C`、括号参数），再看修饰符加 `target`。
pub(super) fn check_annotation_then(lx: &mut ScanLexer, target: &str) -> bool {
    while lx.lookahead() == '@' {
        lx.skip();
        if !is_word_char(lx.lookahead()) {
            return false;
        }
        while is_word_char(lx.lookahead()) {
            lx.skip();
        }
        while lx.lookahead() == '.' {
            lx.skip();
            if !is_word_char(lx.lookahead()) {
                break;
            }
            while is_word_char(lx.lookahead()) {
                lx.skip();
            }
        }
        if lx.lookahead() == '(' {
            let mut depth = 1;
            lx.skip();
            while depth > 0 && lx.lookahead() != '\0' && !lx.eof() {
                if lx.lookahead() == '"' {
                    lx.skip();
                    while lx.lookahead() != '"' && lx.lookahead() != '\0' && !lx.eof() {
                        if lx.lookahead() == '\\' {
                            lx.skip();
                        }
                        lx.skip();
                    }
                    if lx.lookahead() == '"' {
                        lx.skip();
                    }
                } else {
                    if lx.lookahead() == '(' {
                        depth += 1;
                    } else if lx.lookahead() == ')' {
                        depth -= 1;
                    }
                    lx.skip();
                }
            }
        }
        while is_space(lx.lookahead()) {
            lx.skip();
        }
    }
    if is_word_char(lx.lookahead()) && !target.starts_with(lx.lookahead()) {
        return check_modifier_then(lx, target);
    }
    check_word(lx, target)
}

pub(super) fn constructor_space(c: char) -> bool {
    let c = c as u32;
    c == 0x20
        || (9..=13).contains(&c)
        || c == 0x85
        || c == 0xa0
        || c == 0x1680
        || (0x2000..=0x200a).contains(&c)
        || c == 0x2028
        || c == 0x2029
        || c == 0x202f
        || c == 0x205f
        || c == 0x3000
}

/// 只用 advance 的空白与注释探测。
pub(super) fn constructor_trivia(lx: &mut ScanLexer, newline: &mut bool) -> bool {
    loop {
        while constructor_space(lx.lookahead()) {
            if matches!(lx.lookahead(), '\n' | '\r') {
                *newline = true;
            }
            lx.next();
        }
        if lx.lookahead() != '/' {
            return true;
        }
        lx.next();
        if lx.lookahead() == '/' {
            while !lx.eof() && !matches!(lx.lookahead(), '\n' | '\r') {
                lx.next();
            }
        } else if lx.lookahead() == '*' {
            lx.next();
            let mut depth = 1;
            while depth > 0 && !lx.eof() {
                let c = lx.lookahead();
                if matches!(c, '\n' | '\r') {
                    *newline = true;
                }
                lx.next();
                if c == '/' && lx.lookahead() == '*' {
                    lx.next();
                    depth += 1;
                } else if c == '*' && lx.lookahead() == '/' {
                    lx.next();
                    depth -= 1;
                }
            }
            if depth > 0 {
                return false;
            }
        } else {
            return false;
        }
    }
}

pub(super) fn constructor_word_char(c: char) -> bool {
    if (c as u32) < 128 {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    !constructor_space(c)
}

pub(super) fn scan_try_clause_keyword(lx: &mut ScanLexer, suffix: &str) -> bool {
    scan_for_word(lx, suffix) && !constructor_word_char(lx.lookahead())
}

/// 读一个完整标识符，返回（内容, 是否纯 ASCII 且非空）；内容按官方的 C 字符串截断。
pub(super) fn constructor_word(lx: &mut ScanLexer, capacity: usize) -> (Vec<u8>, bool) {
    let mut word = Vec::new();
    let mut ascii = true;
    while !lx.eof() && constructor_word_char(lx.lookahead()) {
        let c = lx.lookahead();
        if c as u32 >= 128 || word.len() + 1 >= capacity {
            ascii = false;
        }
        if word.len() + 1 < capacity {
            word.push(c as u32 as u8);
        }
        lx.next();
    }
    if let Some(p) = word.iter().position(|b| *b == 0) {
        word.truncate(p);
    }
    let ok = ascii && !word.is_empty();
    (word, ok)
}

pub(super) fn constructor_modifier(word: &[u8]) -> bool {
    const WORDS: &[&[u8]] = &[
        b"public",
        b"private",
        b"protected",
        b"internal",
        b"actual",
        b"expect",
        b"final",
        b"open",
        b"abstract",
        b"sealed",
        b"const",
        b"override",
        b"lateinit",
        b"tailrec",
        b"operator",
        b"infix",
        b"inline",
        b"external",
        b"suspend",
        b"data",
        b"inner",
        b"value",
        b"annotation",
        b"noinline",
        b"crossinline",
        b"vararg",
    ];
    WORDS.contains(&word)
}

struct Frame {
    end: char,
    kind: u8,
    dollars: u32,
}

/// 带字符串、字符、反引号标识符、注释和插值的括号配平探测；显式栈，不递归。
pub(super) fn constructor_balanced(lx: &mut ScanLexer, close: char) -> bool {
    let mut stack = vec![Frame {
        end: close,
        kind: 0,
        dollars: 0,
    }];
    lx.next();
    while !stack.is_empty() && !lx.eof() {
        let last = stack.len() - 1;
        let (fend, fkind, fdollars) = (stack[last].end, stack[last].kind, stack[last].dollars);
        let mut c = lx.lookahead();
        if fkind != 0 {
            if fkind == 1 && c == '\\' {
                lx.next();
                if !lx.eof() {
                    lx.next();
                }
                continue;
            }
            if c == fend {
                lx.next();
                if fkind != 2 {
                    stack.pop();
                    continue;
                }
                if lx.lookahead() == '"' {
                    lx.next();
                    if lx.lookahead() == '"' {
                        lx.next();
                        stack.pop();
                    }
                }
                continue;
            }
            if fend == '"' && c == '$' {
                let mut run = 0;
                while lx.lookahead() == '$' {
                    lx.next();
                    run += 1;
                }
                if run >= fdollars && lx.lookahead() == '{' {
                    lx.next();
                    stack.push(Frame {
                        end: '}',
                        kind: 0,
                        dollars: 0,
                    });
                }
                continue;
            }
            lx.next();
            continue;
        }
        if c == fend {
            lx.next();
            stack.pop();
            continue;
        }
        if matches!(c, '(' | '[' | '{') || (c == '<' && fend == '>') {
            let e = match c {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                _ => '>',
            };
            stack.push(Frame {
                end: e,
                kind: 0,
                dollars: 0,
            });
            lx.next();
            continue;
        }
        if c == '/' {
            constructor_trivia(lx, &mut false);
            continue;
        }
        let mut dollars = 1;
        if c == '$' {
            dollars = 0;
            while lx.lookahead() == '$' {
                lx.next();
                dollars += 1;
            }
            c = lx.lookahead();
        }
        if matches!(c, '"' | '\'' | '`') {
            let mut kind = if c == '`' { 3 } else { 1 };
            lx.next();
            if c == '"' && lx.lookahead() == '"' {
                lx.next();
                if lx.lookahead() == '"' {
                    lx.next();
                    kind = 2;
                } else {
                    continue;
                }
            }
            stack.push(Frame {
                end: c,
                kind,
                dollars,
            });
            continue;
        }
        lx.next();
    }
    stack.is_empty()
}

pub(super) fn constructor_annotation(lx: &mut ScanLexer) -> bool {
    lx.next();
    let mut nl = false;
    if !constructor_trivia(lx, &mut nl) {
        return false;
    }
    if lx.lookahead() == '[' {
        return constructor_balanced(lx, ']');
    }
    loop {
        if !constructor_trivia(lx, &mut nl) {
            return false;
        }
        if lx.lookahead() == '`' {
            lx.next();
            while !lx.eof() && lx.lookahead() != '`' {
                lx.next();
            }
            if lx.eof() {
                return false;
            }
            lx.next();
        } else {
            if !constructor_word_char(lx.lookahead()) {
                return false;
            }
            constructor_word(lx, 32);
        }
        if !constructor_trivia(lx, &mut nl) {
            return false;
        }
        if lx.lookahead() == ':' {
            lx.next();
            if !constructor_trivia(lx, &mut nl) {
                return false;
            }
            if lx.lookahead() == '[' {
                return constructor_balanced(lx, ']');
            }
            continue;
        }
        if lx.lookahead() == '.' {
            lx.next();
            continue;
        }
        break;
    }
    if !constructor_trivia(lx, &mut nl) {
        return false;
    }
    if lx.lookahead() == '<' && !constructor_balanced(lx, '>') {
        return false;
    }
    if !constructor_trivia(lx, &mut nl) {
        return false;
    }
    if lx.lookahead() == '(' && !constructor_balanced(lx, ')') {
        return false;
    }
    true
}

/// 逐字符吃掉 `constructor` 并确认标识符在此结束。
pub(super) fn eat_constructor(lx: &mut ScanLexer) -> bool {
    for ch in "constructor".chars() {
        if lx.lookahead() != ch {
            return false;
        }
        lx.next();
    }
    !constructor_word_char(lx.lookahead())
}

pub(super) fn scan_import_dot(lx: &mut ScanLexer) -> bool {
    if lx.lookahead() != '.' {
        return false;
    }
    lx.mark_end();
    lx.next();
    let mut found_newline = false;
    while is_space(lx.lookahead()) {
        if matches!(lx.lookahead(), '\n' | '\r') {
            found_newline = true;
        }
        lx.skip();
    }
    if found_newline && lx.lookahead() == 'i' && scan_for_word(lx, "mport") {
        lx.set_result(AUTOMATIC_SEMICOLON as u16);
        return true;
    }
    lx.set_result(IMPORT_DOT as u16);
    lx.mark_end();
    true
}

/// 换行加注释之后，下一个真正的记号是否续接上一条语句：续接返回假（不插分号）。
pub(super) fn asi_after_comment(lx: &mut ScanLexer, v: &[bool]) -> bool {
    match lx.lookahead() {
        '.' | ',' | '*' | '%' | '>' | '<' | '=' | '{' | '?' | '|' | '&' | '/' => false,
        ':' => {
            lx.skip();
            lx.lookahead() == ':'
        }
        '!' => {
            lx.skip();
            lx.lookahead() != '='
        }
        'e' => {
            if scan_for_word(lx, "lse") {
                return followed_by_arrow(lx);
            }
            true
        }
        'a' => !scan_for_word(lx, "s"),
        'w' => {
            if scan_for_word(lx, "here") {
                return followed_by(lx, '(');
            }
            true
        }
        'c' => !(v[TRY_CLAUSE_HINT] && scan_try_clause_keyword(lx, "atch")),
        'b' => !(v[BY_DELEGATION_HINT] && scan_for_word(lx, "y")),
        'f' => {
            if v[BACKING_FIELD_HINT] && scan_try_clause_keyword(lx, "ield") {
                return false;
            }
            !(v[TRY_CLAUSE_HINT] && scan_try_clause_keyword(lx, "inally"))
        }
        _ => true,
    }
}

/// 与上面相同的判断，但只用 advance：待重放的注释要在原位置重新读。
pub(super) fn continuation_after_block_comment(lx: &mut ScanLexer, v: &[bool]) -> bool {
    let mut nl = false;
    if !constructor_trivia(lx, &mut nl) {
        return !lx.eof();
    }
    match lx.lookahead() {
        '.' | ',' | '*' | '%' | '>' | '<' | '=' | '{' | '?' | '|' | '&' => return true,
        ':' => {
            lx.next();
            return lx.lookahead() != ':';
        }
        '!' => {
            lx.next();
            return lx.lookahead() == '=';
        }
        _ => {}
    }
    let (word, ok) = constructor_word(lx, 32);
    if !ok {
        return false;
    }
    match word.as_slice() {
        b"as" => true,
        b"by" => v[BY_DELEGATION_HINT],
        b"where" => constructor_trivia(lx, &mut nl) && lx.lookahead() != '(',
        b"catch" | b"finally" => v[TRY_CLAUSE_HINT],
        b"field" => v[BACKING_FIELD_HINT],
        b"else" => {
            if !constructor_trivia(lx, &mut nl) || lx.lookahead() != '-' {
                return true;
            }
            lx.next();
            lx.lookahead() != '>'
        }
        _ => false,
    }
}
