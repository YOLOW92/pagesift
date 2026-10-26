//! 内容流和 CMap 共用的词法器：把字节流切成"操作数 + 操作符"。
//! 不递归，嵌套深度有上限，任何输入都不会 panic。

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Operand {
    Num(f64),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Operand>),
    /// 内联字典（BDC 的属性表）：只留顶层的键和标量值，嵌套的值记为 Other。
    Dict(Vec<(Vec<u8>, Operand)>),
    Other,
}

impl Operand {
    pub(crate) fn num(&self) -> Option<f64> {
        match self {
            Operand::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// 字典里某个键的值。
    pub(crate) fn get(&self, key: &[u8]) -> Option<&Operand> {
        match self {
            Operand::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Op {
    pub(crate) name: Vec<u8>,
    pub(crate) args: Vec<Operand>,
}

const MAX_ARRAY_DEPTH: usize = 16;
const MAX_OPERANDS: usize = 4096;

pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

fn is_white(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

enum Token {
    Operand(Operand),
    ArrayStart,
    ArrayEnd,
    Keyword(Vec<u8>),
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Lexer { data, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn skip_white(&mut self) {
        while let Some(b) = self.peek() {
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn regular(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_white(b) || is_delim(b) {
                break;
            }
            self.pos += 1;
        }
        &self.data[start..self.pos]
    }

    fn literal_string(&mut self) -> Vec<u8> {
        // 调用时 pos 指向 '(' 之后
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return out;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        // 调用时 pos 指向 '<' 之后
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex_val(b) {
                match hi.take() {
                    Some(h) => out.push(h << 4 | v),
                    None => hi = Some(v),
                }
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    fn name(&mut self) -> Vec<u8> {
        let raw = self.regular();
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'#'
                && let (Some(h), Some(l)) = (
                    raw.get(i + 1).copied().and_then(hex_val),
                    raw.get(i + 2).copied().and_then(hex_val),
                )
            {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
            out.push(raw[i]);
            i += 1;
        }
        out
    }

    /// 读 `<<` 之后的字典：顶层的 `/键 标量` 留下，数组和嵌套字典跳过记为 Other。
    fn dict(&mut self) -> Operand {
        let mut out = Vec::new();
        loop {
            self.skip_white();
            let Some(b) = self.peek() else { break };
            if b == b'>' && self.data.get(self.pos + 1) == Some(&b'>') {
                self.pos += 2;
                break;
            }
            if b != b'/' {
                // 不是键：当成一个值跳过
                self.skip_value();
                continue;
            }
            self.pos += 1;
            let key = self.name();
            self.skip_white();
            let value = match self.peek() {
                Some(b'(') => {
                    self.pos += 1;
                    Operand::Str(self.literal_string())
                }
                Some(b'<') if self.data.get(self.pos + 1) != Some(&b'<') => {
                    self.pos += 1;
                    Operand::Str(self.hex_string())
                }
                Some(b'/') => {
                    self.pos += 1;
                    Operand::Name(self.name())
                }
                Some(b'>') if self.data.get(self.pos + 1) == Some(&b'>') => continue,
                Some(_) => {
                    let start = self.pos;
                    let word = self.regular();
                    if !word.is_empty()
                        && (word[0].is_ascii_digit() || matches!(word[0], b'+' | b'-' | b'.'))
                    {
                        Operand::Num(parse_number(word))
                    } else {
                        self.pos = start;
                        self.skip_value();
                        Operand::Other
                    }
                }
                None => break,
            };
            out.push((key, value));
        }
        Operand::Dict(out)
    }

    /// 跳过一个值（字符串、数组、嵌套字典或普通词）。
    fn skip_value(&mut self) {
        let Some(b) = self.peek() else { return };
        match b {
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                self.skip_dict();
            }
            b'<' => {
                self.pos += 1;
                self.hex_string();
            }
            b'(' => {
                self.pos += 1;
                self.literal_string();
            }
            b'[' => {
                self.pos += 1;
                let mut depth = 1usize;
                while depth > 0 {
                    self.skip_white();
                    match self.peek() {
                        None => return,
                        Some(b'[') => {
                            self.pos += 1;
                            depth += 1;
                        }
                        Some(b']') => {
                            self.pos += 1;
                            depth -= 1;
                        }
                        Some(_) => self.skip_value(),
                    }
                }
            }
            _ if is_delim(b) => self.pos += 1,
            _ => {
                if self.regular().is_empty() {
                    self.pos += 1;
                }
            }
        }
    }

    /// 跳过 `<< ... >>`。
    fn skip_dict(&mut self) {
        let mut depth = 1usize;
        while depth > 0 {
            self.skip_white();
            let Some(b) = self.peek() else { return };
            match b {
                b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                    self.pos += 2;
                    depth += 1;
                }
                b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    depth -= 1;
                }
                b'<' => {
                    self.pos += 1;
                    self.hex_string();
                }
                b'(' => {
                    self.pos += 1;
                    self.literal_string();
                }
                _ if is_delim(b) => self.pos += 1,
                _ => {
                    self.regular();
                }
            }
        }
    }

    fn token(&mut self) -> Option<Token> {
        loop {
            self.skip_white();
            let b = self.peek()?;
            match b {
                b'(' => {
                    self.pos += 1;
                    return Some(Token::Operand(Operand::Str(self.literal_string())));
                }
                b'<' => {
                    if self.data.get(self.pos + 1) == Some(&b'<') {
                        self.pos += 2;
                        return Some(Token::Operand(self.dict()));
                    }
                    self.pos += 1;
                    return Some(Token::Operand(Operand::Str(self.hex_string())));
                }
                b'/' => {
                    self.pos += 1;
                    return Some(Token::Operand(Operand::Name(self.name())));
                }
                b'[' => {
                    self.pos += 1;
                    return Some(Token::ArrayStart);
                }
                b']' => {
                    self.pos += 1;
                    return Some(Token::ArrayEnd);
                }
                b'>' | b')' | b'{' | b'}' => {
                    self.pos += 1;
                }
                _ => {
                    let word = self.regular();
                    if word.is_empty() {
                        self.pos += 1;
                        continue;
                    }
                    let first = word[0];
                    if first.is_ascii_digit() || matches!(first, b'+' | b'-' | b'.') {
                        return Some(Token::Operand(Operand::Num(parse_number(word))));
                    }
                    if matches!(word, b"true" | b"false" | b"null") {
                        return Some(Token::Operand(Operand::Other));
                    }
                    return Some(Token::Keyword(word.to_vec()));
                }
            }
        }
    }

    /// 内联图像：`BI ... ID <二进制> EI`，跳过二进制数据。
    fn skip_inline_image(&mut self) {
        // 先找 ID
        loop {
            match self.token() {
                None => return,
                Some(Token::Keyword(k)) if k == b"ID" => break,
                Some(_) => {}
            }
        }
        // ID 后面跟一个空白字节
        self.pos += 1;
        let data = self.data;
        let mut i = self.pos;
        while i + 1 < data.len() {
            if data[i] == b'E'
                && data[i + 1] == b'I'
                && (i == 0 || is_white(data[i - 1]))
                && data.get(i + 2).is_none_or(|&c| is_white(c))
            {
                self.pos = i + 2;
                return;
            }
            i += 1;
        }
        self.pos = data.len();
    }

    pub(crate) fn next_op(&mut self) -> Option<Op> {
        let mut args: Vec<Operand> = Vec::new();
        // 数组嵌套用显式栈，不递归
        let mut stack: Vec<Vec<Operand>> = Vec::new();
        loop {
            let tok = self.token()?;
            match tok {
                Token::Operand(o) => {
                    if let Some(top) = stack.last_mut() {
                        top.push(o);
                    } else {
                        if args.len() >= MAX_OPERANDS {
                            args.clear();
                        }
                        args.push(o);
                    }
                }
                Token::ArrayStart => {
                    if stack.len() < MAX_ARRAY_DEPTH {
                        stack.push(Vec::new());
                    } else if let Some(top) = stack.last_mut() {
                        top.push(Operand::Other);
                    }
                }
                Token::ArrayEnd => {
                    if let Some(done) = stack.pop() {
                        let arr = Operand::Array(done);
                        if let Some(top) = stack.last_mut() {
                            top.push(arr);
                        } else {
                            args.push(arr);
                        }
                    }
                }
                Token::Keyword(k) => {
                    // 没闭合的数组按已读到的内容收尾
                    while let Some(done) = stack.pop() {
                        let arr = Operand::Array(done);
                        if let Some(top) = stack.last_mut() {
                            top.push(arr);
                        } else {
                            args.push(arr);
                        }
                    }
                    if k == b"BI" {
                        self.skip_inline_image();
                    }
                    return Some(Op { name: k, args });
                }
            }
        }
    }
}

fn parse_number(word: &[u8]) -> f64 {
    // 宽松解析：容忍 "--5"、"1.2.3" 这类写法
    let mut s = String::with_capacity(word.len());
    let mut seen_dot = false;
    for (i, &b) in word.iter().enumerate() {
        match b {
            b'+' | b'-' if i == 0 => s.push(b as char),
            b'+' | b'-' => {}
            b'.' if !seen_dot => {
                seen_dot = true;
                s.push('.');
            }
            b'0'..=b'9' => s.push(b as char),
            _ => break,
        }
    }
    let v = s.parse::<f64>().unwrap_or(0.0);
    if v.is_finite() { v } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(src: &[u8]) -> Vec<Op> {
        let mut lx = Lexer::new(src);
        let mut v = Vec::new();
        while let Some(op) = lx.next_op() {
            v.push(op);
        }
        v
    }

    #[test]
    fn basic_text_ops() {
        let v = ops(b"BT /F1 12 Tf 72 700 Td (Hello \\(x\\)) Tj [(A) -120 <42>] TJ ET");
        let names: Vec<_> = v.iter().map(|o| o.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                b"BT".to_vec(),
                b"Tf".to_vec(),
                b"Td".to_vec(),
                b"Tj".to_vec(),
                b"TJ".to_vec(),
                b"ET".to_vec()
            ]
        );
        assert_eq!(v[3].args, vec![Operand::Str(b"Hello (x)".to_vec())]);
        assert_eq!(
            v[4].args,
            vec![Operand::Array(vec![
                Operand::Str(b"A".to_vec()),
                Operand::Num(-120.0),
                Operand::Str(b"B".to_vec())
            ])]
        );
    }

    #[test]
    fn inline_image_skipped() {
        let v = ops(b"q BI /W 2 /H 2 ID \x00 EIx\xff EI Q (x) Tj");
        let names: Vec<_> = v.iter().map(|o| o.name.clone()).collect();
        assert_eq!(
            names,
            vec![b"q".to_vec(), b"BI".to_vec(), b"Q".to_vec(), b"Tj".to_vec()]
        );
    }

    #[test]
    fn octal_and_names() {
        let v = ops(b"/A#20B (\\101\\1) Tj");
        assert_eq!(v[0].args[0], Operand::Name(b"A B".to_vec()));
        assert_eq!(v[0].args[1], Operand::Str(vec![b'A', 1]));
    }
}
