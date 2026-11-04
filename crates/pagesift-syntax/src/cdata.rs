//! 读取 tree-sitter 生成的 `parser.c`：词法切分、常量解析、初始化列表解析。
//!
//! 这里把 C 源码只当数据，不执行任何东西。

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Id(String),
    Num(i64),
    Str(String),
    P(&'static str),
}

const PUNCTS: [&str; 14] = [
    "<=", ">=", "==", "!=", "&&", "||", "->", "<", ">", "!", "=", "&", "|", "-",
];

/// 把 C 源码切成记号，跳过注释和预处理行（`#define` 另行收集）。
pub fn tokenize(src: &str) -> Result<(Vec<Tok>, HashMap<String, i64>), String> {
    let b = src.as_bytes();
    let (mut i, mut toks, mut defs) = (0usize, Vec::new(), HashMap::new());
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b'#' {
            let st = i;
            while i < b.len() && b[i] != b'\n' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            let line = &src[st..i.min(b.len())];
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[0] == "#define" {
                if let Some(v) = parse_int(parts[2]) {
                    defs.insert(parts[1].to_string(), v);
                }
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(Tok::Id(src[st..i].to_string()));
        } else if c.is_ascii_digit() {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let t = src[st..i].trim_end_matches(['u', 'U', 'l', 'L']);
            toks.push(Tok::Num(
                parse_int(t).ok_or_else(|| format!("数字无法解析: {t}"))?,
            ));
        } else if c == b'"' {
            let mut s = Vec::new();
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    let (ch, n) = escape(&b[i..])?;
                    s.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                    i += n;
                } else {
                    s.push(b[i]);
                    i += 1;
                }
            }
            i += 1;
            toks.push(Tok::Str(String::from_utf8_lossy(&s).into_owned()));
        } else if c == b'\'' {
            i += 1;
            let ch: u32;
            if b[i] == b'\\' {
                i += 1;
                let (c2, n) = escape(&b[i..])?;
                ch = c2 as u32;
                i += n;
            } else {
                let s = &src[i..];
                let c2 = s.chars().next().ok_or("字符字面量被截断")?;
                ch = c2 as u32;
                i += c2.len_utf8();
            }
            if b.get(i) != Some(&b'\'') {
                return Err("字符字面量没有结尾".into());
            }
            i += 1;
            toks.push(Tok::Num(ch as i64));
        } else {
            let rest = &src[i..];
            if let Some(p) = PUNCTS.iter().find(|p| rest.starts_with(**p)) {
                toks.push(Tok::P(p));
                i += p.len();
            } else {
                let p: &'static str = match c {
                    b'{' => "{",
                    b'}' => "}",
                    b'(' => "(",
                    b')' => ")",
                    b'[' => "[",
                    b']' => "]",
                    b',' => ",",
                    b';' => ";",
                    b':' => ":",
                    b'.' => ".",
                    b'*' => "*",
                    b'+' => "+",
                    b'~' => "~",
                    b'?' => "?",
                    b'/' => "/",
                    _ => return Err(format!("无法识别的字符 {:?}", c as char)),
                };
                toks.push(Tok::P(p));
                i += 1;
            }
        }
    }
    Ok((toks, defs))
}

fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn escape(b: &[u8]) -> Result<(char, usize), String> {
    let c = *b.first().ok_or("转义被截断")?;
    Ok(match c {
        b'n' => ('\n', 1),
        b't' => ('\t', 1),
        b'r' => ('\r', 1),
        b'0' => ('\0', 1),
        b'a' => ('\x07', 1),
        b'b' => ('\x08', 1),
        b'f' => ('\x0c', 1),
        b'v' => ('\x0b', 1),
        b'\\' | b'\'' | b'"' | b'?' => (c as char, 1),
        b'x' | b'u' | b'U' => {
            let mut n = 1;
            let mut v = 0u32;
            while let Some(d) = b.get(n).and_then(|x| (*x as char).to_digit(16)) {
                v = v.wrapping_mul(16).wrapping_add(d);
                n += 1;
                if c != b'x' && n > if c == b'u' { 4 } else { 8 } {
                    break;
                }
            }
            (char::from_u32(v).unwrap_or('\u{fffd}'), n)
        }
        _ => return Err(format!("未知转义 \\{}", c as char)),
    })
}

/// 初始化列表里的值。
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Int(i64),
    Str(String),
    /// `NAME(args)`，如 `REDUCE(...)`、`SHIFT(3)`。
    Call(String, Vec<Val>),
    /// 带下标/字段名的列表项，已把位置项补成连续下标。
    List(Vec<(Key, Val)>),
    Id(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Idx(i64),
    Field(String),
}

impl Val {
    pub fn int(&self) -> Option<i64> {
        if let Val::Int(i) = self {
            Some(*i)
        } else {
            None
        }
    }
    pub fn list(&self) -> &[(Key, Val)] {
        if let Val::List(l) = self { l } else { &[] }
    }
    pub fn field(&self, name: &str) -> Option<&Val> {
        self.list()
            .iter()
            .find(|(k, _)| matches!(k, Key::Field(f) if f == name))
            .map(|(_, v)| v)
    }
}

pub struct Src {
    pub toks: Vec<Tok>,
    pub consts: HashMap<String, i64>,
}

impl Src {
    pub fn new(text: &str) -> Result<Src, String> {
        let (toks, mut consts) = tokenize(text)?;
        consts.insert("ts_builtin_sym_end".into(), 0);
        consts.insert("true".into(), 1);
        consts.insert("false".into(), 0);
        consts.insert("NULL".into(), 0);
        let mut s = Src { toks, consts };
        s.collect_enums()?;
        Ok(s)
    }

    /// 收集 `enum name { a = 1, b = 2 }` 里的常量。
    fn collect_enums(&mut self) -> Result<(), String> {
        let mut i = 0;
        while i < self.toks.len() {
            if self.toks[i] == Tok::Id("enum".into()) {
                let mut j = i + 1;
                while j < self.toks.len()
                    && self.toks[j] != Tok::P("{")
                    && self.toks[j] != Tok::P(";")
                {
                    j += 1;
                }
                if self.toks.get(j) == Some(&Tok::P("{")) {
                    j += 1;
                    let mut next = 0i64;
                    while j < self.toks.len() && self.toks[j] != Tok::P("}") {
                        if let Tok::Id(n) = self.toks[j].clone() {
                            j += 1;
                            if self.toks.get(j) == Some(&Tok::P("=")) {
                                if let Some(Tok::Num(v)) = self.toks.get(j + 1) {
                                    next = *v;
                                }
                                j += 2;
                            }
                            self.consts.insert(n, next);
                            next += 1;
                        } else {
                            j += 1;
                        }
                    }
                    i = j;
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// 找 `name[` 开头的声明，返回 `=` 之后初始化器的起点。
    pub fn find_init(&self, name: &str) -> Option<usize> {
        let mut i = 0;
        while i + 1 < self.toks.len() {
            if self.toks[i] == Tok::Id(name.into()) && matches!(self.toks[i + 1], Tok::P("[")) {
                let mut j = i + 1;
                while j < self.toks.len()
                    && self.toks[j] != Tok::P("=")
                    && self.toks[j] != Tok::P(";")
                {
                    j += 1;
                }
                if self.toks.get(j) == Some(&Tok::P("=")) {
                    return Some(j + 1);
                }
            }
            i += 1;
        }
        None
    }

    pub fn parse_init_at(&self, pos: usize) -> Result<Val, String> {
        let mut p = pos;
        self.init(&mut p)
    }

    pub fn array(&self, name: &str) -> Result<Val, String> {
        let p = self
            .find_init(name)
            .ok_or_else(|| format!("找不到数组 {name}"))?;
        self.parse_init_at(p)
    }

    fn expr_int(&self, p: &mut usize) -> Result<i64, String> {
        match self.toks.get(*p) {
            Some(Tok::Num(n)) => {
                *p += 1;
                Ok(*n)
            }
            Some(Tok::P("-")) => {
                *p += 1;
                Ok(-self.expr_int(p)?)
            }
            // 类型转换 `(TSStateId)(-1)` 与括号表达式
            Some(Tok::P("(")) => {
                *p += 1;
                if matches!(self.toks.get(*p), Some(Tok::Id(_)))
                    && self.toks.get(*p + 1) == Some(&Tok::P(")"))
                {
                    *p += 2;
                    return self.expr_int(p);
                }
                let v = self.expr_int(p)?;
                self.expect(p, ")")?;
                Ok(v)
            }
            Some(Tok::Id(n)) if self.toks.get(*p + 1) == Some(&Tok::P("(")) => {
                *p += 2;
                let v = self.expr_int(p)?;
                self.expect(p, ")")?;
                match n.as_str() {
                    "SMALL_STATE" => {
                        Ok(v - self.consts.get("LARGE_STATE_COUNT").copied().unwrap_or(0))
                    }
                    _ => Ok(v),
                }
            }
            Some(Tok::Id(n)) => {
                *p += 1;
                self.consts
                    .get(n)
                    .copied()
                    .ok_or_else(|| format!("未知常量 {n}"))
            }
            t => Err(format!("期望整数表达式，遇到 {t:?}")),
        }
    }

    fn init(&self, p: &mut usize) -> Result<Val, String> {
        match self.toks.get(*p) {
            Some(Tok::P("{")) => {
                *p += 1;
                let mut items = Vec::new();
                let mut next = 0i64;
                loop {
                    match self.toks.get(*p) {
                        Some(Tok::P("}")) => {
                            *p += 1;
                            break;
                        }
                        Some(Tok::P(",")) => *p += 1,
                        None => return Err("初始化列表没有结尾".into()),
                        _ => {
                            let mut key = None;
                            while matches!(self.toks.get(*p), Some(Tok::P("[")) | Some(Tok::P(".")))
                            {
                                if self.toks[*p] == Tok::P("[") {
                                    *p += 1;
                                    let v = self.expr_int(p)?;
                                    self.expect(p, "]")?;
                                    key = Some(Key::Idx(v));
                                } else {
                                    *p += 1;
                                    if let Some(Tok::Id(f)) = self.toks.get(*p) {
                                        key = Some(Key::Field(f.clone()));
                                        *p += 1;
                                    }
                                }
                            }
                            if key.is_some() {
                                self.expect(p, "=")?;
                            }
                            let v = self.init(p)?;
                            let k = match key {
                                Some(Key::Idx(i)) => {
                                    next = i + 1;
                                    Key::Idx(i)
                                }
                                Some(f) => f,
                                None => {
                                    next += 1;
                                    Key::Idx(next - 1)
                                }
                            };
                            items.push((k, v));
                        }
                    }
                }
                Ok(Val::List(items))
            }
            Some(Tok::Str(s)) => {
                *p += 1;
                Ok(Val::Str(s.clone()))
            }
            Some(Tok::Id(n)) if self.toks.get(*p + 1) == Some(&Tok::P("(")) => {
                *p += 2;
                let mut args = Vec::new();
                while self.toks.get(*p) != Some(&Tok::P(")")) {
                    if self.toks.get(*p) == Some(&Tok::P(",")) {
                        *p += 1;
                        continue;
                    }
                    if *p >= self.toks.len() {
                        return Err("宏调用没有结尾".into());
                    }
                    args.push(self.init(p)?);
                }
                *p += 1;
                Ok(Val::Call(n.clone(), args))
            }
            Some(Tok::Id(n)) => {
                if let Some(v) = self.consts.get(n) {
                    *p += 1;
                    Ok(Val::Int(*v))
                } else {
                    *p += 1;
                    Ok(Val::Id(n.clone()))
                }
            }
            Some(_) => Ok(Val::Int(self.expr_int(p)?)),
            None => Err("意外结束".into()),
        }
    }

    pub fn expect(&self, p: &mut usize, s: &str) -> Result<(), String> {
        if matches!(self.toks.get(*p), Some(Tok::P(x)) if *x == s) {
            *p += 1;
            Ok(())
        } else {
            Err(format!("期望 {s}，遇到 {:?}", self.toks.get(*p)))
        }
    }
}
