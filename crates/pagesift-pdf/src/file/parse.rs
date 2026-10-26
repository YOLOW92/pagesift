//! 对象语法（ISO 32000-1 §7.2–7.3）：词法 + 直接对象。
//!
//! 规范写法的结果与 lopdf 逐位相同（整数 i64、实数 f32、转义与十六进制串的规则一致）。
//! 不规范的写法按 pdf.js 的思路放宽：数组/字典里认不出的词跳过，十六进制串里的非法字符忽略，
//! 截断的对象尽量返回已经读到的部分，而不是整个对象作废。

use super::{Dictionary, Object, ObjectId, StringFormat};

/// 数组/字典的最大嵌套深度，防止恶意输入把栈打穿。
const MAX_DEPTH: usize = 100;

pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\0' | b'\x0C')
}

pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

/// 结束一个对象、不能被当成普通词跳过的关键字。
fn is_terminator(word: &[u8]) -> bool {
    matches!(
        word,
        b"endobj" | b"stream" | b"endstream" | b"obj" | b"xref" | b"trailer" | b"startxref"
    )
}

enum Token<'a> {
    Value(Object),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    Keyword(&'a [u8]),
    Eof,
}

pub(crate) struct Parser<'a> {
    data: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(data: &'a [u8], pos: usize) -> Self {
        Parser {
            data,
            pos: pos.min(data.len()),
        }
    }

    /// 跳过空白和注释。
    pub(crate) fn skip_ws(&mut self) {
        while let Some(&b) = self.data.get(self.pos) {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(&c) = self.data.get(self.pos) {
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// 读一个普通词（数字或关键字），不前进时返回空。
    fn word(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.data.get(self.pos).is_some_and(|&b| is_regular(b)) {
            self.pos += 1;
        }
        &self.data[start..self.pos]
    }

    /// 读一个关键字，是给定的词就前进并返回 true。
    pub(crate) fn eat_keyword(&mut self, kw: &[u8]) -> bool {
        self.skip_ws();
        let save = self.pos;
        if self.word() == kw {
            true
        } else {
            self.pos = save;
            false
        }
    }

    /// 读一个非负整数词（对象号、代号、xref 里的数字）。
    pub(crate) fn uint(&mut self) -> Option<u64> {
        self.skip_ws();
        let save = self.pos;
        let w = self.word();
        if !w.is_empty() && w.len() <= 19 && w.iter().all(u8::is_ascii_digit) {
            // 19 位以内不会溢出 u64
            Some(w.iter().fold(0u64, |v, &d| v * 10 + u64::from(d - b'0')))
        } else {
            self.pos = save;
            None
        }
    }

    /// `N G obj`，返回对象号和代号，位置停在 `obj` 之后。
    pub(crate) fn object_header(&mut self) -> Option<ObjectId> {
        let save = self.pos;
        let id = (|| {
            let num = u32::try_from(self.uint()?).ok()?;
            let generation = u16::try_from(self.uint()?).ok()?;
            self.eat_keyword(b"obj").then_some((num, generation))
        })();
        if id.is_none() {
            self.pos = save;
        }
        id
    }

    fn token(&mut self) -> Token<'a> {
        self.skip_ws();
        let Some(&b) = self.data.get(self.pos) else {
            return Token::Eof;
        };
        match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Token::DictStart
            }
            b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::DictEnd
            }
            b'<' => Token::Value(self.hex_string()),
            b'(' => Token::Value(self.literal_string()),
            b'/' => Token::Value(Object::Name(self.name())),
            b'>' | b')' | b'{' | b'}' => {
                // 落单的分隔符：当成一个认不出的词
                self.pos += 1;
                Token::Keyword(&self.data[self.pos - 1..self.pos])
            }
            _ => {
                let w = self.word();
                match w {
                    b"true" => Token::Value(Object::Boolean(true)),
                    b"false" => Token::Value(Object::Boolean(false)),
                    b"null" => Token::Value(Object::Null),
                    _ => match number(w) {
                        Some(n) => Token::Value(n),
                        None => Token::Keyword(w),
                    },
                }
            }
        }
    }

    /// 读一个直接对象；`N G R` 读成引用。读不出对象时返回 None，位置不前进。
    pub(crate) fn object(&mut self) -> Option<Object> {
        self.object_at_depth(0)
    }

    fn object_at_depth(&mut self, depth: usize) -> Option<Object> {
        let save = self.pos;
        match self.token() {
            Token::Value(Object::Integer(n)) if n >= 0 => Some(self.maybe_reference(n)),
            Token::Value(v) => Some(v),
            Token::ArrayStart => Some(self.array(depth + 1)),
            Token::DictStart => Some(Object::Dictionary(self.dict_body(depth + 1))),
            Token::ArrayEnd | Token::DictEnd | Token::Keyword(_) | Token::Eof => {
                self.pos = save;
                None
            }
        }
    }

    /// 刚读到非负整数 `n`：后面跟着 `G R` 就是引用。
    fn maybe_reference(&mut self, n: i64) -> Object {
        let save = self.pos;
        if let (Ok(num), Some(generation)) = (u32::try_from(n), self.uint())
            && let Ok(generation) = u16::try_from(generation)
            && self.eat_keyword(b"R")
        {
            return Object::Reference((num, generation));
        }
        self.pos = save;
        Object::Integer(n)
    }

    fn array(&mut self, depth: usize) -> Object {
        let mut items = Vec::new();
        if depth > MAX_DEPTH {
            return Object::Null;
        }
        loop {
            let save = self.pos;
            match self.token() {
                Token::ArrayEnd | Token::Eof => break,
                Token::Value(Object::Integer(n)) if n >= 0 => items.push(self.maybe_reference(n)),
                Token::Value(v) => items.push(v),
                Token::ArrayStart => items.push(self.array(depth + 1)),
                Token::DictStart => items.push(Object::Dictionary(self.dict_body(depth + 1))),
                // 数组没闭合就遇到字典结束：让外层处理
                Token::DictEnd => {
                    self.pos = save;
                    break;
                }
                Token::Keyword(w) if is_terminator(w) => {
                    self.pos = save;
                    break;
                }
                Token::Keyword(_) => {}
            }
        }
        Object::Array(items)
    }

    /// 读字典 `<<` 之后的部分，到 `>>` 为止。
    pub(crate) fn dict_body(&mut self, depth: usize) -> Dictionary {
        let mut dict = Dictionary::default();
        if depth > MAX_DEPTH {
            return dict;
        }
        loop {
            let save = self.pos;
            match self.token() {
                Token::DictEnd | Token::Eof => break,
                Token::Value(Object::Name(key)) => match self.object_at_depth(depth) {
                    Some(value) => dict.push(key, value),
                    // 键后面没有值（`/Key >>` 或截断）：丢掉这个键
                    None => {
                        let save = self.pos;
                        match self.token() {
                            Token::Keyword(w) if !is_terminator(w) => {}
                            _ => self.pos = save,
                        }
                        if self.peek_terminator() {
                            break;
                        }
                    }
                },
                Token::Keyword(w) if is_terminator(w) => {
                    self.pos = save;
                    break;
                }
                // 不是名字的键：跳过（嵌套的数组/字典整个读掉）
                Token::ArrayStart => {
                    self.array(depth + 1);
                }
                Token::DictStart => {
                    self.dict_body(depth + 1);
                }
                Token::Value(_) | Token::ArrayEnd | Token::Keyword(_) => {}
            }
        }
        dict
    }

    fn peek_terminator(&mut self) -> bool {
        self.skip_ws();
        let save = self.pos;
        let w = self.word();
        self.pos = save;
        is_terminator(w)
    }

    fn name(&mut self) -> Vec<u8> {
        self.pos += 1; // '/'
        let mut out = Vec::new();
        while let Some(&b) = self.data.get(self.pos) {
            if !is_regular(b) {
                break;
            }
            if b == b'#'
                && let (Some(h), Some(l)) = (
                    self.data.get(self.pos + 1).and_then(|&c| hex_value(c)),
                    self.data.get(self.pos + 2).and_then(|&c| hex_value(c)),
                )
            {
                out.push(h << 4 | l);
                self.pos += 3;
                continue;
            }
            out.push(b);
            self.pos += 1;
        }
        out
    }

    fn hex_string(&mut self) -> Object {
        self.pos += 1; // '<'
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let Some(v) = hex_value(b) else { continue };
            match high.take() {
                Some(h) => out.push(h << 4 | v),
                None => high = Some(v),
            }
        }
        if let Some(h) = high {
            out.push(h << 4);
        }
        Object::String(out, StringFormat::Hexadecimal)
    }

    /// 字面串：括号可嵌套；行尾原样保留（加密串的密文里可能有裸 CR，不能改）。
    fn literal_string(&mut self) -> Object {
        self.pos += 1; // '('
        let mut out = Vec::new();
        let mut depth = 0usize;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    out.push(b);
                }
                b'\\' => self.escape(&mut out),
                _ => out.push(b),
            }
        }
        Object::String(out, StringFormat::Literal)
    }

    fn escape(&mut self, out: &mut Vec<u8>) {
        let Some(&c) = self.data.get(self.pos) else {
            return;
        };
        self.pos += 1;
        match c {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'0'..=b'7' => {
                let mut v = u16::from(c - b'0');
                for _ in 0..2 {
                    match self.data.get(self.pos) {
                        Some(&d @ b'0'..=b'7') => {
                            v = v * 8 + u16::from(d - b'0');
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                // 规范要求忽略溢出
                out.push(v as u8);
            }
            // 续行：反斜杠后的行尾不算内容
            b'\r' => {
                if self.data.get(self.pos) == Some(&b'\n') {
                    self.pos += 1;
                }
            }
            b'\n' => {}
            _ => out.push(c),
        }
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 数字词：`[+-]?digits`、`[+-]?digits.digits*`、`[+-]?.digits`。
/// 整数溢出 i64 时按实数读。多余的前导符号（`--5`）和尾部杂字（`0.5-`）按 pdf.js 的做法容忍。
pub(crate) fn number(w: &[u8]) -> Option<Object> {
    let signs = w.iter().take_while(|&&b| b == b'+' || b == b'-').count();
    let negative = w[..signs].contains(&b'-');
    let rest = &w[signs..];
    let mut dot = false;
    let len = rest
        .iter()
        .take_while(|&&b| {
            if b == b'.' && !dot {
                dot = true;
                true
            } else {
                b.is_ascii_digit()
            }
        })
        .count();
    let body = &rest[..len];
    if !body.iter().any(u8::is_ascii_digit) {
        return None;
    }
    // 尾部有杂字时只认干净的写法以外的少数情形：杂字全是符号或点
    if len < rest.len()
        && !rest[len..]
            .iter()
            .all(|&b| b == b'-' || b == b'+' || b == b'.')
    {
        return None;
    }
    let mut text = String::with_capacity(body.len() + 1);
    if negative {
        text.push('-');
    }
    text.push_str(std::str::from_utf8(body).ok()?);
    if !dot && let Ok(i) = text.parse::<i64>() {
        return Some(Object::Integer(i));
    }
    text.parse::<f32>().ok().map(Object::Real)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &[u8]) -> Option<Object> {
        Parser::new(src, 0).object()
    }

    #[test]
    fn numbers_match_pdf_grammar() {
        assert_eq!(parse(b"42"), Some(Object::Integer(42)));
        assert_eq!(parse(b"-17"), Some(Object::Integer(-17)));
        assert_eq!(parse(b"+5"), Some(Object::Integer(5)));
        assert_eq!(parse(b"3."), Some(Object::Real(3.0)));
        assert_eq!(parse(b"-.5"), Some(Object::Real(-0.5)));
        assert_eq!(parse(b"0.25"), Some(Object::Real(0.25)));
        assert_eq!(parse(b"--5"), Some(Object::Integer(-5)));
        assert_eq!(parse(b"99999999999999999999"), Some(Object::Real(1e20)));
    }

    #[test]
    fn references_need_generation_and_r() {
        assert_eq!(parse(b"12 0 R"), Some(Object::Reference((12, 0))));
        assert_eq!(
            parse(b"[1 2 3 0 R 4]"),
            Some(Object::Array(vec![
                Object::Integer(1),
                Object::Integer(2),
                Object::Reference((3, 0)),
                Object::Integer(4),
            ]))
        );
    }

    #[test]
    fn strings_and_names() {
        assert_eq!(
            parse(
                br"(a\(b\)\101\
c)"
            ),
            Some(Object::String(b"a(b)Ac".to_vec(), StringFormat::Literal))
        );
        assert_eq!(
            parse(b"<48 65 6C6c 6>"),
            Some(Object::String(b"Hell`".to_vec(), StringFormat::Hexadecimal))
        );
        assert_eq!(parse(b"/A#20B#zz"), Some(Object::Name(b"A B#zz".to_vec())));
    }

    #[test]
    fn lenient_dictionaries() {
        let Some(Object::Dictionary(d)) = parse(b"<< /A 1 junk /B [1 2 /C >> /D (x) >>") else {
            panic!("not a dict");
        };
        assert_eq!(d.get(b"A"), Some(&Object::Integer(1)));
        assert!(matches!(d.get(b"B"), Some(Object::Array(a)) if a.len() == 3));
        // 数组没闭合，遇到 >> 时字典也结束
        assert_eq!(d.get(b"D"), None);
        let Some(Object::Dictionary(d)) = parse(b"<</Length 5 0 R/Filter/FlateDecode>>stream")
        else {
            panic!("not a dict");
        };
        assert_eq!(d.get(b"Length"), Some(&Object::Reference((5, 0))));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = "[".repeat(10_000);
        assert!(parse(deep.as_bytes()).is_some());
    }
}
