//! 把 `ts_lex` / `ts_lex_keywords` 里的 C 状态机翻译成数据表。
//!
//! 每个词法状态的语句序列（ADVANCE、SKIP、ACCEPT_TOKEN、字符集比较）
//! 被摊平成“码点区间 → (已接受符号, 动作, 目标状态)”，运行时只做二分查找。

use crate::cdata::{Src, Tok, Val};
use crate::table::{EOF_CP, LexTable, NO_SYM, Seg};
use std::collections::HashMap;

type Set = Vec<(u32, u32)>;

fn union(a: &Set, b: &Set) -> Set {
    let mut v: Set = a.iter().chain(b.iter()).copied().collect();
    v.sort_unstable();
    let mut out: Set = Vec::new();
    for (lo, hi) in v {
        if let Some(l) = out.last_mut()
            && lo <= l.1.saturating_add(1)
        {
            l.1 = l.1.max(hi);
            continue;
        }
        out.push((lo, hi));
    }
    out
}

fn complement(a: &Set) -> Set {
    let mut out = Vec::new();
    let mut next = 0u32;
    for &(lo, hi) in a {
        if lo > next {
            out.push((next, lo - 1));
        }
        next = hi + 1;
    }
    if next <= EOF_CP {
        out.push((next, EOF_CP));
    }
    out
}

fn intersect(a: &Set, b: &Set) -> Set {
    complement(&union(&complement(a), &complement(b)))
}

fn contains(s: &Set, x: u32) -> bool {
    let i = s.partition_point(|r| r.1 < x);
    i < s.len() && s[i].0 <= x
}

#[derive(Clone, Copy)]
enum Act {
    Adv(u16),
    Skip(u16),
    Acc(u16),
}

enum St {
    If(Set, Act),
    Do(Act),
    Map(Vec<(u32, u16)>),
}

struct P<'a> {
    t: &'a [Tok],
    p: usize,
    src: &'a Src,
    sets: &'a mut HashMap<String, Set>,
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p)
    }
    fn eat(&mut self, s: &str) -> Result<(), String> {
        match self.peek() {
            Some(Tok::P(x)) if *x == s => {
                self.p += 1;
                Ok(())
            }
            t => Err(format!(
                "词法状态机: 期望 {s}，遇到 {t:?} (记号位置 {})",
                self.p
            )),
        }
    }
    fn is_p(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::P(x)) if *x == s)
    }
    fn num(&mut self) -> Result<i64, String> {
        match self.peek() {
            Some(Tok::Num(n)) => {
                let n = *n;
                self.p += 1;
                Ok(n)
            }
            Some(Tok::Id(i)) => {
                let v = *self
                    .src
                    .consts
                    .get(i)
                    .ok_or_else(|| format!("未知常量 {i}"))?;
                self.p += 1;
                Ok(v)
            }
            t => Err(format!("期望数字，遇到 {t:?}")),
        }
    }
    fn or(&mut self) -> Result<Set, String> {
        let mut a = self.and()?;
        while self.is_p("||") {
            self.p += 1;
            let b = self.and()?;
            a = union(&a, &b);
        }
        Ok(a)
    }
    fn and(&mut self) -> Result<Set, String> {
        let mut a = self.atom()?;
        while self.is_p("&&") {
            self.p += 1;
            let b = self.atom()?;
            a = intersect(&a, &b);
        }
        Ok(a)
    }
    fn atom(&mut self) -> Result<Set, String> {
        if self.is_p("(") {
            self.p += 1;
            let s = self.or()?;
            self.eat(")")?;
            return Ok(s);
        }
        if self.is_p("!") {
            self.p += 1;
            let s = self.atom()?;
            return Ok(complement(&s));
        }
        match self.peek() {
            Some(Tok::Id(i)) if i == "eof" => {
                self.p += 1;
                return Ok(vec![(EOF_CP, EOF_CP)]);
            }
            Some(Tok::Id(i)) if i == "set_contains" => {
                self.p += 1;
                self.eat("(")?;
                let name = match self.peek() {
                    Some(Tok::Id(n)) => n.clone(),
                    t => return Err(format!("set_contains 名字: {t:?}")),
                };
                self.p += 1;
                if !self.sets.contains_key(&name) {
                    let v = self.src.array(&name)?;
                    let mut s: Set = Vec::new();
                    for (_, r) in v.list() {
                        let l = r.list();
                        if l.len() == 2
                            && let (Some(a), Some(b)) = (l[0].1.int(), l[1].1.int())
                        {
                            s.push((a as u32, b as u32));
                        }
                    }
                    s.sort_unstable();
                    self.sets.insert(name.clone(), union(&s, &Vec::new()));
                }
                // 跳过 ", 数量, lookahead )"
                while !self.is_p(")") {
                    self.p += 1;
                }
                self.eat(")")?;
                return Ok(self.sets[&name].clone());
            }
            _ => {}
        }
        // 比较式：operand op operand，其中一边是 lookahead
        let (lhs_look, lhs) = self.operand()?;
        let op = match self.peek() {
            Some(Tok::P(o)) => *o,
            t => return Err(format!("期望比较运算符，遇到 {t:?}")),
        };
        self.p += 1;
        let (rhs_look, rhs) = self.operand()?;
        let (c, look_left) = match (lhs_look, rhs_look) {
            (true, false) => (rhs, true),
            (false, true) => (lhs, false),
            _ => return Err("比较式里必须有且仅有一个 lookahead".into()),
        };
        let c = c as u32;
        let s: Set = match (op, look_left) {
            ("==", _) => vec![(c, c)],
            ("!=", _) => complement(&vec![(c, c)]),
            ("<=", true) | (">=", false) => vec![(0, c)],
            ("<=", false) | (">=", true) => vec![(c, EOF_CP)],
            ("<", true) | (">", false) => {
                if c == 0 {
                    vec![]
                } else {
                    vec![(0, c - 1)]
                }
            }
            ("<", false) | (">", true) => vec![(c + 1, EOF_CP)],
            _ => return Err(format!("不支持的运算符 {op}")),
        };
        Ok(s)
    }
    fn operand(&mut self) -> Result<(bool, i64), String> {
        if matches!(self.peek(), Some(Tok::Id(i)) if i == "lookahead") {
            self.p += 1;
            return Ok((true, 0));
        }
        Ok((false, self.num()?))
    }
    fn act(&mut self) -> Result<Act, String> {
        let name = match self.peek() {
            Some(Tok::Id(n)) => n.clone(),
            t => return Err(format!("期望动作，遇到 {t:?}")),
        };
        self.p += 1;
        self.eat("(")?;
        let n = self.num()? as u16;
        self.eat(")")?;
        match name.as_str() {
            "ADVANCE" => Ok(Act::Adv(n)),
            "SKIP" => Ok(Act::Skip(n)),
            "ACCEPT_TOKEN" => Ok(Act::Acc(n)),
            o => Err(format!("未知动作 {o}")),
        }
    }
}

/// 转换一个词法函数。`name` 是 `ts_lex` 或 `ts_lex_keywords`。
pub fn convert_lex(
    src: &Src,
    name: &str,
    sets: &mut HashMap<String, Set>,
) -> Result<LexTable, String> {
    let t = &src.toks;
    let mut i = 0;
    let start = loop {
        if i + 3 >= t.len() {
            return Ok(LexTable::default()); // 没有这个函数
        }
        if t[i] == Tok::Id("bool".into())
            && t[i + 1] == Tok::Id(name.into())
            && t[i + 2] == Tok::P("(")
        {
            break i;
        }
        i += 1;
    };
    let mut p = P {
        t,
        p: start,
        src,
        sets,
    };
    while !(matches!(p.peek(), Some(Tok::Id(s)) if s == "switch")) {
        p.p += 1;
        if p.p >= t.len() {
            return Err("找不到 switch".into());
        }
    }
    while !p.is_p("{") {
        p.p += 1;
    }
    p.p += 1;
    let mut states: Vec<(usize, Vec<St>)> = Vec::new();
    loop {
        match p.peek() {
            Some(Tok::Id(c)) if c == "case" => {
                p.p += 1;
                let n = p.num()? as usize;
                p.eat(":")?;
                let mut body = Vec::new();
                loop {
                    match p.peek() {
                        Some(Tok::Id(s)) if s == "END_STATE" => {
                            p.p += 1;
                            p.eat("(")?;
                            p.eat(")")?;
                            p.eat(";")?;
                            break;
                        }
                        Some(Tok::Id(s)) if s == "if" => {
                            p.p += 1;
                            p.eat("(")?;
                            let mut set = p.or()?;
                            p.eat(")")?;
                            // 官方语义：条件在 lookahead==0 时对 EOF 也成立（eof 原子本身除外）
                            if !(set.len() == 1 && set[0] == (EOF_CP, EOF_CP)) {
                                let has0 = contains(&set, 0);
                                set = intersect(&set, &vec![(0, EOF_CP - 1)]);
                                if has0 {
                                    set = union(&set, &vec![(EOF_CP, EOF_CP)]);
                                }
                            }
                            let a = p.act()?;
                            p.eat(";")?;
                            body.push(St::If(set, a));
                        }
                        Some(Tok::Id(s)) if s == "ADVANCE_MAP" => {
                            p.p += 1;
                            p.eat("(")?;
                            let mut m = Vec::new();
                            while !p.is_p(")") {
                                let k = p.num()? as u32;
                                p.eat(",")?;
                                let v = p.num()? as u16;
                                p.eat(",")?;
                                m.push((k, v));
                            }
                            p.eat(")")?;
                            p.eat(";")?;
                            body.push(St::Map(m));
                        }
                        Some(Tok::Id(_)) => {
                            let a = p.act()?;
                            p.eat(";")?;
                            body.push(St::Do(a));
                        }
                        t => return Err(format!("状态 {n} 里遇到意外记号 {t:?}")),
                    }
                }
                states.push((n, body));
            }
            _ => break,
        }
    }
    let max = states.iter().map(|s| s.0).max().map_or(0, |m| m + 1);
    let mut table = LexTable {
        starts: vec![0; max + 1],
        segs: Vec::new(),
    };
    let mut by_state: Vec<Option<&Vec<St>>> = vec![None; max];
    for (n, b) in &states {
        by_state[*n] = Some(b);
    }
    for (n, slot) in by_state.iter().enumerate() {
        table.starts[n] = table.segs.len() as u32;
        let body = match slot {
            Some(b) => *b,
            None => {
                table.segs.push(Seg {
                    lo: 0,
                    accept: NO_SYM,
                    kind: 0,
                    target: 0,
                });
                continue;
            }
        };
        let mut cuts: Vec<u32> = vec![0];
        for s in body {
            match s {
                St::If(set, _) => {
                    for &(lo, hi) in set {
                        cuts.push(lo);
                        cuts.push(hi + 1);
                    }
                }
                St::Map(m) => {
                    for &(k, _) in m {
                        cuts.push(k);
                        cuts.push(k + 1);
                    }
                }
                St::Do(_) => {}
            }
        }
        cuts.retain(|c| *c <= EOF_CP);
        cuts.sort_unstable();
        cuts.dedup();
        let mut last: Option<Seg> = None;
        for &lo in &cuts {
            let seg = eval(body, lo);
            let seg = Seg { lo, ..seg };
            match last {
                Some(l)
                    if l.accept == seg.accept && l.kind == seg.kind && l.target == seg.target => {}
                _ => {
                    table.segs.push(seg);
                    last = Some(seg);
                }
            }
        }
    }
    table.starts[max] = table.segs.len() as u32;
    Ok(table)
}

fn eval(body: &[St], x: u32) -> Seg {
    let mut accept = NO_SYM;
    let fin = |accept: u16, kind: u8, target: u16| Seg {
        lo: 0,
        accept,
        kind,
        target,
    };
    for s in body {
        let act = match s {
            St::If(set, a) => {
                if contains(set, x) {
                    Some(*a)
                } else {
                    None
                }
            }
            St::Do(a) => Some(*a),
            St::Map(m) => m.iter().find(|(k, _)| *k == x).map(|(_, v)| Act::Adv(*v)),
        };
        match act {
            Some(Act::Acc(sym)) => accept = sym,
            Some(Act::Adv(t)) => return fin(accept, 1, t),
            Some(Act::Skip(t)) => return fin(accept, 2, t),
            None => {}
        }
    }
    fin(accept, 0, 0)
}

#[allow(dead_code)]
fn _unused(_: &Val) {}
