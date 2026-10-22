use crate::Tag;

use super::{
    AttrOp, AttrSel, CASE_INSENSITIVE_ATTRS, Comb, Complex, Compound, MAX_NESTING, Nth, Selector,
    SelectorError, Simple,
};

pub(super) struct Parser<'a> {
    pub(super) s: &'a str,
    pub(super) b: &'a [u8],
    pub(super) pos: usize,
    pub(super) nesting: u32,
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_name(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == b'-'
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn peek_at(&self, i: usize) -> Option<u8> {
        self.b.get(self.pos + i).copied()
    }

    fn err(&self, message: impl Into<String>) -> SelectorError {
        SelectorError {
            offset: self.pos,
            message: message.into(),
        }
    }

    pub(super) fn unexpected(&self) -> SelectorError {
        match self.s[self.pos..].chars().next() {
            Some(c) => self.err(format!("unexpected `{c}`")),
            None => self.err("unexpected end of selector"),
        }
    }

    /// 跳过空白和注释，返回是否有空白（注释本身不算空白）。
    fn skip_ws(&mut self) -> bool {
        let mut ws = false;
        loop {
            match self.peek() {
                Some(c) if is_ws(c) => {
                    ws = true;
                    self.pos += 1;
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.pos = match memchr::memmem::find(&self.b[self.pos + 2..], b"*/") {
                        Some(i) => self.pos + 2 + i + 2,
                        None => self.b.len(),
                    };
                }
                _ => return ws,
            }
        }
    }

    /// 选择器列表，返回列表和匹配深度。`nested` 时以 `)` 结束（不消耗）。
    pub(super) fn list(
        &mut self,
        relative: bool,
        nested: bool,
    ) -> Result<(Selector, u32), SelectorError> {
        let mut list = Vec::new();
        let mut depth = 0;
        loop {
            self.skip_ws();
            let (c, d) = self.complex(relative)?;
            list.push(c);
            depth = depth.max(d);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') if nested => break,
                None if !nested => break,
                None => return Err(self.err("expected `)`")),
                _ => return Err(self.unexpected()),
            }
        }
        Ok((
            Selector {
                list: list.into_boxed_slice(),
            },
            depth,
        ))
    }

    fn combinator(&mut self) -> Option<Comb> {
        let c = match self.peek()? {
            b'>' => Comb::Child,
            b'+' => Comb::Next,
            b'~' => Comb::Later,
            _ => return None,
        };
        self.pos += 1;
        Some(c)
    }

    /// 复杂选择器。`relative` 时可以以组合器开头（`:has(> a)`），左端补一个锚点。
    fn complex(&mut self, relative: bool) -> Result<(Complex, u32), SelectorError> {
        // 从左往右收集，最后反转成从右往左
        let mut parts: Vec<(Vec<Simple>, u32)> = Vec::new();
        let mut combs: Vec<Comb> = Vec::new();
        if relative {
            let comb = self.combinator().unwrap_or(Comb::Descendant);
            self.skip_ws();
            parts.push((vec![Simple::Anchor], 0));
            combs.push(comb);
        }
        parts.push(self.compound()?);
        loop {
            let ws = self.skip_ws();
            let comb = match self.combinator() {
                Some(c) => {
                    self.skip_ws();
                    c
                }
                None => match self.peek() {
                    None | Some(b',' | b')') => break,
                    Some(_) if ws => Comb::Descendant,
                    Some(_) => return Err(self.unexpected()),
                },
            };
            combs.push(comb);
            parts.push(self.compound()?);
        }
        let depth = parts.iter().map(|(_, d)| 1 + d).sum();
        let n = parts.len();
        let parts = parts
            .into_iter()
            .rev()
            .enumerate()
            .map(|(i, (mut simples, _))| {
                simples.sort_by_key(Simple::cost);
                let tag = simples.iter().find_map(|s| match s {
                    Simple::Type(t, _) if *t != Tag::Other => Some(*t),
                    _ => None,
                });
                Compound {
                    tag,
                    simples: simples.into_boxed_slice(),
                    // 最左边的复合选择器没有左邻，组合器不会被用到
                    comb: if i + 1 < n {
                        combs[n - 2 - i]
                    } else {
                        Comb::Descendant
                    },
                }
            })
            .collect();
        Ok((Complex { parts }, depth))
    }

    /// 复合选择器，返回简单选择器和其中嵌套选择器的最大匹配深度。
    fn compound(&mut self) -> Result<(Vec<Simple>, u32), SelectorError> {
        let mut simples = Vec::new();
        let mut depth = 0;
        let mut any = false;
        match self.peek() {
            Some(b'*') => {
                self.pos += 1;
                any = true;
            }
            _ if self.ident_start() => {
                let name = self.ident()?.to_ascii_lowercase();
                simples.push(Simple::Type(Tag::from_name(&name), name.into()));
                any = true;
            }
            _ => {}
        }
        if self.peek() == Some(b'|') {
            return Err(self.err("namespaces are not supported"));
        }
        loop {
            match self.peek() {
                Some(b'#') => {
                    self.pos += 1;
                    if !self.ident_start() {
                        return Err(self.err("expected an identifier after `#`"));
                    }
                    simples.push(Simple::Id(self.ident()?.into()));
                }
                Some(b'.') => {
                    self.pos += 1;
                    if !self.ident_start() {
                        return Err(self.err("expected an identifier after `.`"));
                    }
                    simples.push(Simple::Class(self.ident()?.into()));
                }
                Some(b'[') => {
                    self.pos += 1;
                    simples.push(self.attr()?);
                }
                Some(b':') => {
                    self.pos += 1;
                    let (s, d) = self.pseudo()?;
                    depth = depth.max(d);
                    simples.extend(s);
                }
                _ => break,
            }
            any = true;
        }
        if !any {
            return Err(match self.peek() {
                None => self.err("expected a selector"),
                Some(_) => self.unexpected(),
            });
        }
        Ok((simples, depth))
    }

    fn attr(&mut self) -> Result<Simple, SelectorError> {
        self.skip_ws();
        if matches!(self.peek(), Some(b'*' | b'|')) {
            return Err(self.err("namespaces are not supported"));
        }
        if !self.ident_start() {
            return Err(self.err("expected an attribute name"));
        }
        let name = self.ident()?.to_ascii_lowercase();
        if self.peek() == Some(b'|') && self.peek_at(1) != Some(b'=') {
            return Err(self.err("namespaces are not supported"));
        }
        self.skip_ws();
        let op = match (self.peek(), self.peek_at(1)) {
            (Some(b']'), _) => {
                self.pos += 1;
                return Ok(Simple::Attr(Box::new(AttrSel {
                    name: name.into(),
                    op: AttrOp::Exists,
                    value: "".into(),
                    ci: false,
                })));
            }
            (Some(b'='), _) => AttrOp::Equals,
            (Some(b'~'), Some(b'=')) => AttrOp::Includes,
            (Some(b'|'), Some(b'=')) => AttrOp::Dash,
            (Some(b'^'), Some(b'=')) => AttrOp::Prefix,
            (Some(b'$'), Some(b'=')) => AttrOp::Suffix,
            (Some(b'*'), Some(b'=')) => AttrOp::Substring,
            (None, _) => return Err(self.err("unclosed attribute selector, expected `]`")),
            _ => return Err(self.unexpected()),
        };
        self.pos += if op == AttrOp::Equals { 1 } else { 2 };
        self.skip_ws();
        let value = match self.peek() {
            Some(q @ (b'"' | b'\'')) => self.string(q)?,
            _ if self.ident_start() => self.ident()?,
            None => return Err(self.err("expected an attribute value")),
            _ => {
                return Err(
                    self.err("expected an identifier or a quoted string as the attribute value")
                );
            }
        };
        self.skip_ws();
        let mut ci = CASE_INSENSITIVE_ATTRS.contains(&name.as_str());
        if self.ident_start() {
            let flag_at = self.pos;
            let flag = self.ident()?;
            if flag.eq_ignore_ascii_case("i") {
                ci = true;
            } else if flag.eq_ignore_ascii_case("s") {
                ci = false;
            } else {
                self.pos = flag_at;
                return Err(self.err(format!("unknown attribute selector flag `{flag}`")));
            }
            self.skip_ws();
        }
        match self.peek() {
            Some(b']') => self.pos += 1,
            None => return Err(self.err("unclosed attribute selector, expected `]`")),
            _ => return Err(self.unexpected()),
        }
        let op = match op {
            AttrOp::Includes if value.is_empty() || value.bytes().any(is_ws) => AttrOp::Never,
            AttrOp::Prefix | AttrOp::Suffix | AttrOp::Substring if value.is_empty() => {
                AttrOp::Never
            }
            op => op,
        };
        Ok(Simple::Attr(Box::new(AttrSel {
            name: name.into(),
            op,
            value: value.into(),
            ci,
        })))
    }

    /// `:` 之后的伪类。`:only-child` 这类展开成两个简单选择器。
    fn pseudo(&mut self) -> Result<(Vec<Simple>, u32), SelectorError> {
        if self.peek() == Some(b':') {
            return Err(self.err("pseudo-elements are not supported"));
        }
        let start = self.pos;
        if !self.ident_start() {
            return Err(self.err("expected a pseudo-class name after `:`"));
        }
        let name = self.ident()?.to_ascii_lowercase();
        if self.peek() == Some(b'(') {
            self.pos += 1;
            return self.pseudo_fn(&name, start);
        }
        let s = match name.as_str() {
            "first-child" => vec![Nth::simple(0, 1, false, false)],
            "last-child" => vec![Nth::simple(0, 1, true, false)],
            "only-child" => vec![
                Nth::simple(0, 1, false, false),
                Nth::simple(0, 1, true, false),
            ],
            "first-of-type" => vec![Nth::simple(0, 1, false, true)],
            "last-of-type" => vec![Nth::simple(0, 1, true, true)],
            "only-of-type" => vec![
                Nth::simple(0, 1, false, true),
                Nth::simple(0, 1, true, true),
            ],
            "empty" => vec![Simple::Empty],
            "root" => vec![Simple::Root],
            "scope" => vec![Simple::Scope],
            _ => {
                self.pos = start;
                return Err(self.err(format!("unsupported pseudo-class `:{name}`")));
            }
        };
        Ok((s, 0))
    }

    fn pseudo_fn(&mut self, name: &str, start: usize) -> Result<(Vec<Simple>, u32), SelectorError> {
        let nested = |p: &mut Self, relative: bool| -> Result<(Selector, u32), SelectorError> {
            if p.nesting >= MAX_NESTING {
                return Err(p.err(format!("selector nested deeper than {MAX_NESTING} levels")));
            }
            p.nesting += 1;
            let r = p.list(relative, true);
            p.nesting -= 1;
            r
        };
        let (simple, depth) = match name {
            "not" => {
                let (s, d) = nested(self, false)?;
                (Simple::Not(s), d)
            }
            "is" | "where" | "matches" | "any" => {
                let (s, d) = nested(self, false)?;
                (Simple::Is(s), d)
            }
            "has" => {
                let (s, d) = nested(self, true)?;
                (Simple::Has(s), d)
            }
            "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                self.skip_ws();
                let (a, b) = self.an_plus_b()?;
                let ws = self.skip_ws();
                let mut nth = Nth {
                    a,
                    b,
                    last: name.starts_with("nth-last"),
                    of_type: name.ends_with("of-type"),
                    of: None,
                };
                let mut depth = 0;
                if !nth.of_type && ws && self.ident_start() {
                    let at = self.pos;
                    if self.ident()?.eq_ignore_ascii_case("of") && self.skip_ws() {
                        let (s, d) = nested(self, false)?;
                        nth.of = Some(s);
                        depth = d;
                    } else {
                        self.pos = at;
                        return Err(self.unexpected());
                    }
                }
                (Simple::Nth(Box::new(nth)), depth)
            }
            _ => {
                self.pos = start;
                return Err(self.err(format!("unsupported pseudo-class `:{name}()`")));
            }
        };
        self.skip_ws();
        match self.peek() {
            Some(b')') => self.pos += 1,
            None => return Err(self.err(format!("unclosed `:{name}(`, expected `)`"))),
            _ => return Err(self.unexpected()),
        }
        Ok((vec![simple], depth))
    }

    /// `An+B` 记法：`odd`、`even`、`3`、`-n+2`、`2n + 1` 等。
    fn an_plus_b(&mut self) -> Result<(i32, i32), SelectorError> {
        let start = self.pos;
        let bad = |p: &mut Self| {
            p.pos = start;
            Err(p.err("invalid An+B expression"))
        };
        if self.ident_start() {
            let at = self.pos;
            let word = self.ident()?;
            if word.eq_ignore_ascii_case("odd") {
                return Ok((2, 1));
            }
            if word.eq_ignore_ascii_case("even") {
                return Ok((2, 0));
            }
            self.pos = at;
        }
        let sign = match self.peek() {
            Some(b'+') => {
                self.pos += 1;
                1
            }
            Some(b'-') => {
                self.pos += 1;
                -1
            }
            _ => 1,
        };
        let digits = self.digits();
        if matches!(self.peek(), Some(b'n' | b'N')) {
            self.pos += 1;
            let a = sign * digits.unwrap_or(1);
            // n 后面紧跟的字符不能再组成标识符（例如 `2nd`）
            if self.peek().is_some_and(|c| is_name(c) && c != b'-') {
                return bad(self);
            }
            let save = self.pos;
            self.skip_ws();
            let bsign = match self.peek() {
                Some(b'+') => 1,
                Some(b'-') => -1,
                _ => {
                    self.pos = save;
                    return Ok((a, 0));
                }
            };
            self.pos += 1;
            self.skip_ws();
            match self.digits() {
                Some(b) => Ok((a, bsign * b)),
                None => bad(self),
            }
        } else {
            match digits {
                Some(b) => Ok((0, sign * b)),
                None => bad(self),
            }
        }
    }

    fn digits(&mut self) -> Option<i32> {
        let start = self.pos;
        let mut v: i32 = 0;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            v = v.saturating_mul(10).saturating_add(i32::from(c - b'0'));
            self.pos += 1;
        }
        (self.pos > start).then_some(v)
    }

    /// 当前位置能否开始一个标识符（CSS 语法的 "would start an identifier"）。
    fn ident_start(&self) -> bool {
        let valid_escape = |i: usize| {
            self.peek_at(i) == Some(b'\\')
                && !matches!(self.peek_at(i + 1), Some(b'\n' | b'\r' | b'\x0c'))
        };
        match self.peek() {
            Some(b'-') => {
                matches!(self.peek_at(1), Some(c) if is_name_start(c) || c == b'-')
                    || valid_escape(1)
            }
            Some(c) if is_name_start(c) => true,
            _ => valid_escape(0),
        }
    }

    fn ident(&mut self) -> Result<String, SelectorError> {
        let mut out = String::new();
        loop {
            match self.peek() {
                Some(b'\\') if !matches!(self.peek_at(1), Some(b'\n' | b'\r' | b'\x0c')) => {
                    self.pos += 1;
                    out.push(self.escape());
                }
                Some(c) if is_name(c) => {
                    let ch = self.s[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
                _ => break,
            }
        }
        if out.is_empty() {
            return Err(self.err("expected an identifier"));
        }
        Ok(out)
    }

    /// 反斜杠之后的转义：1～6 位十六进制（后面可跟一个空白）或任意一个字符。
    fn escape(&mut self) -> char {
        let start = self.pos;
        let mut v: u32 = 0;
        while self.pos - start < 6
            && let Some(c) = self.peek()
            && c.is_ascii_hexdigit()
        {
            v = v * 16 + (c as char).to_digit(16).unwrap_or(0);
            self.pos += 1;
        }
        if self.pos > start {
            if self.peek() == Some(b'\r') && self.peek_at(1) == Some(b'\n') {
                self.pos += 2;
            } else if self.peek().is_some_and(is_ws) {
                self.pos += 1;
            }
            return match char::from_u32(v) {
                Some(c) if v != 0 => c,
                _ => '\u{fffd}',
            };
        }
        match self.s[self.pos..].chars().next() {
            Some(c) => {
                self.pos += c.len_utf8();
                c
            }
            None => '\u{fffd}',
        }
    }

    /// 引号字符串，`q` 是引号字符。到结尾没闭合也接受（和 CSS 语法一致）。
    fn string(&mut self, q: u8) -> Result<String, SelectorError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Ok(out),
                Some(c) if c == q => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\n' | b'\r' | b'\x0c') => {
                    return Err(self.err("unterminated string"));
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        None => {}
                        Some(b'\r') if self.peek_at(1) == Some(b'\n') => self.pos += 2,
                        Some(b'\n' | b'\r' | b'\x0c') => self.pos += 1,
                        Some(_) => out.push(self.escape()),
                    }
                }
                Some(_) => {
                    let ch = self.s[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }
}
