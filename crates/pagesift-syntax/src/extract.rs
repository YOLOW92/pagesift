//! 从语法树提取大纲：函数、类、导入，以及按顶层符号切成的分段。
//!
//! 输入不可信：大小、时间、符号数都有上限，任何失败都返回 `None`，由调用方退回成一整块代码。

use crate::Limits;
use crate::api::{Ev, parse};
use crate::langs::{Cont, Lang, Rule, Sp, is_leading, rule};
use crate::table::Language;

/// 超过这个大小不解析。
const MAX_INPUT: usize = 2 << 20;
/// ERROR 节点覆盖的字节超过文件的这个比例（百分比）就不信任这棵树。
const MAX_ERROR_PERCENT: usize = 20;
const MAX_SYMBOLS: usize = 2000;
const MAX_IMPORTS: usize = 500;
const MAX_TEXT: usize = 200;
const MAX_DEPTH: u32 = 64;
const MAX_MILLIS: u64 = 3000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Type,
    Module,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub kind: SymbolKind,
    /// 没有名字的（匿名命名空间、`export default function`）为空串。
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub depth: u32,
}

/// 字节范围。所有分段首尾相接，正好覆盖整个文件。
#[derive(Clone, Debug)]
pub struct Section {
    pub start: usize,
    pub end: usize,
    /// 形如 `function foo`；文件开头没有符号的部分为 `None`。
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Outline {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<String>,
    pub sections: Vec<Section>,
}

/// 提取大纲。不支持的语言、过大的文件、解析不干净或没有符号的文件返回 `None`。
pub fn outline(tag: &str, source: &str) -> Option<Outline> {
    let lang = Lang::from_tag(tag)?;
    if source.is_empty() || source.len() > MAX_INPUT {
        return None;
    }
    // 输入不可信：运行时的 panic 不能带崩调用方，退回成一整块代码就好
    std::panic::catch_unwind(|| run(lang, source))
        .ok()
        .flatten()
}

fn run(lang: Lang, source: &str) -> Option<Outline> {
    let table = lang.table()?;
    let limits = Limits {
        max_millis: MAX_MILLIS,
        ..Limits::default()
    };
    let tree = parse(table, source.as_bytes(), limits);
    if tree.truncated {
        return None;
    }
    let nodes = Nodes::build(&tree, table)?;
    if nodes.error_bytes * 100 > source.len() * MAX_ERROR_PERCENT {
        return None;
    }
    let mut w = Walker {
        lang,
        source,
        nodes: &nodes,
        lines: line_starts(source),
        out: Outline::default(),
    };
    let mut elements = Vec::new();
    w.flatten(nodes.top(), 0, &mut elements);
    let mut anchors = Vec::new();
    let single_scope = w.single_scope(&elements);
    for (i, &e) in elements.iter().enumerate() {
        let before = w.out.symbols.len();
        let mut first = None;
        if single_scope == Some(i) && w.scope_inner(e, &mut anchors) {
            continue;
        }
        w.walk(e, false, 0, 0, &mut first);
        if w.out.symbols.len() > before
            && let Some(title) = first
        {
            anchors.push((w.anchor_start(&elements, i), nodes.n[e].end, title));
        }
    }
    if w.out.symbols.is_empty() {
        return None;
    }
    let mut out = w.out;
    out.sections = sections(source, anchors);
    Some(out)
}

/// 把锚点换算成首尾相接的分段：符号之间的内容归入后一段，文件末尾的内容归入最后一段。
fn sections(source: &str, anchors: Vec<(usize, usize, String)>) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::with_capacity(anchors.len() + 1);
    let mut at = 0usize;
    for (i, (start, end, title)) in anchors.into_iter().enumerate() {
        if i == 0 && !source.get(..start).unwrap_or("").trim().is_empty() {
            out.push(Section {
                start: 0,
                end: start,
                title: None,
            });
            at = start;
        }
        // 同一行上挨着的两个符号切不开，并进上一段
        if end <= at {
            continue;
        }
        let stop = line_end(source, end);
        out.push(Section {
            start: at,
            end: stop,
            title: Some(title),
        });
        at = stop;
    }
    match out.last_mut() {
        Some(last) => last.end = source.len(),
        None => out.push(Section {
            start: 0,
            end: source.len(),
            title: None,
        }),
    }
    out
}

fn line_end(source: &str, at: usize) -> usize {
    match source
        .as_bytes()
        .get(at..)
        .and_then(|b| b.iter().position(|&c| c == b'\n'))
    {
        Some(p) => at + p + 1,
        None => source.len(),
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(
        source
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i + 1),
    );
    v
}

struct Node {
    kind: &'static str,
    field: Option<&'static str>,
    start: usize,
    end: usize,
    children: Vec<u32>,
}

/// 把遍历事件收成一棵可以随意访问的树。下标 0 是虚拟根。
struct Nodes {
    n: Vec<Node>,
    error_bytes: usize,
}

impl Nodes {
    fn build(tree: &crate::api::ParseTree, table: &'static Language) -> Option<Nodes> {
        let mut n = vec![Node {
            kind: "",
            field: None,
            start: 0,
            end: 0,
            children: Vec::new(),
        }];
        let mut stack = vec![(0u32, false)];
        let mut error_bytes = 0usize;
        tree.visit(table, |ev| match ev {
            Ev::Open(v) => {
                let id = n.len() as u32;
                let (parent, in_error) = *stack.last().unwrap_or(&(0, false));
                if v.error && !in_error {
                    error_bytes += v.end.saturating_sub(v.start);
                }
                n.push(Node {
                    kind: v.kind,
                    field: v.field,
                    start: v.start,
                    end: v.end,
                    children: Vec::new(),
                });
                n[parent as usize].children.push(id);
                stack.push((id, in_error || v.error));
            }
            Ev::Close => {
                stack.pop();
            }
        });
        Some(Nodes { n, error_bytes })
    }

    /// 真正的根：可见的根节点是虚拟根唯一的孩子。
    fn top(&self) -> usize {
        match self.n[0].children.as_slice() {
            [only] => *only as usize,
            _ => 0,
        }
    }

    fn field(&self, id: usize, name: &str) -> Option<usize> {
        self.n[id]
            .children
            .iter()
            .map(|&c| c as usize)
            .find(|&c| self.n[c].field == Some(name))
    }
}

struct Walker<'a> {
    lang: Lang,
    source: &'a str,
    nodes: &'a Nodes,
    lines: Vec<usize>,
    out: Outline,
}

impl Walker<'_> {
    fn text(&self, id: usize) -> &str {
        let n = &self.nodes.n[id];
        self.source.get(n.start..n.end).unwrap_or("")
    }

    /// 压掉空白、截断到上限（在字符边界上）。
    fn clean(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len().min(MAX_TEXT));
        for word in s.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            for c in word.chars() {
                if out.len() + c.len_utf8() > MAX_TEXT {
                    return out;
                }
                out.push(c);
            }
        }
        out
    }

    fn line(&self, at: usize) -> u32 {
        self.lines.partition_point(|&s| s <= at) as u32
    }

    /// 透明层（预处理条件、`extern "C"`）展开成它的子节点。
    fn flatten(&self, id: usize, depth: u32, out: &mut Vec<usize>) {
        for &c in &self.nodes.n[id].children {
            let c = c as usize;
            if matches!(rule(self.lang, self.nodes.n[c].kind), Rule::Flat) && depth < MAX_DEPTH {
                self.flatten(c, depth + 1, out);
            } else {
                out.push(c);
            }
        }
    }

    /// 符号所在分段的起点：所在行的行首，再往前包含紧贴着的注释和属性（中间没有空行）。
    fn anchor_start(&self, elements: &[usize], i: usize) -> usize {
        let mut line = self.line(self.nodes.n[elements[i]].start);
        for &e in elements[..i].iter().rev() {
            let n = &self.nodes.n[e];
            if !is_leading(n.kind) {
                break;
            }
            // 注释结束位置可能带着换行符，取最后一个字节所在的行
            let end_line = self.line(n.end.saturating_sub(1).max(n.start));
            if end_line + 1 != line {
                break;
            }
            line = self.line(n.start);
        }
        self.lines[line.saturating_sub(1) as usize]
    }

    fn name(&self, id: usize) -> String {
        // Rust 的 impl 没有 name，用它实现的类型当名字；C 的函数虽然也有 type 字段，那是返回类型
        let field = if self.nodes.n[id].kind == "impl_item" {
            "type"
        } else {
            "name"
        };
        if let Some(c) = self.nodes.field(id, field) {
            return self.clean(self.text(c));
        }
        // C 系函数：名字藏在 declarator 链的最里面
        let mut at = id;
        for _ in 0..16 {
            let next = self.nodes.field(at, "declarator").or_else(|| {
                self.nodes.n[at]
                    .children
                    .iter()
                    .map(|&c| c as usize)
                    .find(|&c| {
                        let k = self.nodes.n[c].kind;
                        k.ends_with("declarator")
                            || is_identifier(k)
                            || matches!(
                                k,
                                "signature"
                                    | "type_head"
                                    | "call_expression"
                                    | "where_expression"
                                    | "typed_expression"
                                    | "parametrized_type_expression"
                            )
                    })
            });
            match next {
                Some(c) if is_identifier(self.nodes.n[c].kind) => return self.clean(self.text(c)),
                Some(c) => at = c,
                None => break,
            }
        }
        String::new()
    }

    fn push(
        &mut self,
        kind: SymbolKind,
        label: &str,
        name: String,
        id: usize,
        depth: u32,
        first: &mut Option<String>,
    ) {
        if self.out.symbols.len() >= MAX_SYMBOLS {
            return;
        }
        if first.is_none() {
            *first = Some(if name.is_empty() {
                label.to_string()
            } else {
                format!("{label} {name}")
            });
        }
        let (s, e) = (self.nodes.n[id].start, self.nodes.n[id].end);
        let (start_line, end_line) = (self.line(s), self.line(e.saturating_sub(1).max(s)));
        self.out.symbols.push(Symbol {
            kind,
            name,
            start_line,
            end_line,
            depth,
        });
    }

    fn import(&mut self, id: usize) {
        if self.out.imports.len() < MAX_IMPORTS {
            let t = self.clean(self.text(id));
            self.out.imports.push(t);
        }
    }

    fn children(&self, id: usize) -> Vec<usize> {
        self.nodes.n[id]
            .children
            .iter()
            .map(|&c| c as usize)
            .collect()
    }

    /// 顶层只有一个作用域容器（命名空间、`mod`），其余元素都不产生符号时，返回它的下标。
    fn single_scope(&self, elements: &[usize]) -> Option<usize> {
        let mut found = None;
        for (i, &e) in elements.iter().enumerate() {
            match rule(self.lang, self.nodes.n[e].kind) {
                Rule::Sym(_, _, Cont::Scope) if found.is_none() => found = Some(i),
                Rule::Skip | Rule::Import => {}
                _ => return None,
            }
        }
        found
    }

    /// 往作用域容器里切一层：容器自己记一个符号，分段锚点落在它的直接成员上。
    /// 成员里一个符号都没有时撤销并返回假，由调用方按普通符号处理。
    fn scope_inner(&mut self, id: usize, anchors: &mut Vec<(usize, usize, String)>) -> bool {
        let Rule::Sym(k, label, _) = rule(self.lang, self.nodes.n[id].kind) else {
            return false;
        };
        let base = self.out.symbols.len();
        let name = self.name(id);
        self.push(k, label, name, id, 0, &mut None);
        let body = self.nodes.field(id, "body").unwrap_or(id);
        let mut inner = Vec::new();
        self.flatten(body, 1, &mut inner);
        let mut found = Vec::new();
        for (j, &e) in inner.iter().enumerate() {
            let before = self.out.symbols.len();
            let mut first = None;
            self.walk(e, false, 1, 1, &mut first);
            if self.out.symbols.len() > before
                && let Some(title) = first
            {
                found.push((self.anchor_start(&inner, j), self.nodes.n[e].end, title));
            }
        }
        if found.is_empty() {
            self.out.symbols.truncate(base);
            return false;
        }
        anchors.extend(found);
        true
    }

    /// `budget`：还能穿过几层不认识的节点（容器的 body）。顶层为 0，不往普通语句里找符号。
    fn walk(
        &mut self,
        id: usize,
        in_type: bool,
        budget: u32,
        depth: u32,
        first: &mut Option<String>,
    ) {
        if depth >= MAX_DEPTH {
            return;
        }
        let kind = self.nodes.n[id].kind;
        match rule(self.lang, kind) {
            Rule::Sym(k, label, cont) => {
                let needs_body = self.lang.is_c_family()
                    && matches!(
                        kind,
                        "struct_specifier" | "class_specifier" | "enum_specifier"
                    );
                if needs_body && self.nodes.field(id, "body").is_none() {
                    return;
                }
                let (k, label) = if k == SymbolKind::Function && in_type {
                    (SymbolKind::Method, "method")
                } else {
                    (k, label)
                };
                let name = self.name(id);
                self.push(k, label, name, id, depth, first);
                if !matches!(cont, Cont::No) {
                    for c in self.children(id) {
                        self.walk(c, matches!(cont, Cont::Type), 1, depth + 1, first);
                    }
                }
            }
            Rule::Wrap | Rule::Flat => {
                for c in self.children(id) {
                    self.walk(c, in_type, budget, depth + 1, first);
                }
            }
            Rule::Import => self.import(id),
            Rule::Special(sp) => self.special(sp, id, depth, first),
            Rule::Skip => {
                if budget > 0 {
                    for c in self.children(id) {
                        self.walk(c, in_type, budget - 1, depth + 1, first);
                    }
                }
            }
        }
    }

    fn special(&mut self, sp: Sp, id: usize, depth: u32, first: &mut Option<String>) {
        match sp {
            Sp::GoImport => self.go_imports(id, 0),
            Sp::ExCall => self.ex_call(id, depth, first),
            Sp::RAssign => {
                // `name <- function(...) ...`
                let lhs = self.nodes.field(id, "lhs");
                let rhs = self.nodes.field(id, "rhs");
                if let (Some(l), Some(r)) = (lhs, rhs)
                    && self.nodes.n[r].kind == "function_definition"
                    && self.nodes.n[l].kind == "identifier"
                {
                    let name = self.clean(self.text(l));
                    self.push(SymbolKind::Function, "function", name, id, depth, first);
                }
            }
            Sp::GoType => {
                for c in self.children(id) {
                    if !matches!(self.nodes.n[c].kind, "type_spec" | "type_alias") {
                        continue;
                    }
                    let shape = self.nodes.field(c, "type").map(|t| self.nodes.n[t].kind);
                    let (k, label) = match shape {
                        Some("struct_type") => (SymbolKind::Struct, "struct"),
                        Some("interface_type") => (SymbolKind::Interface, "interface"),
                        _ => (SymbolKind::Type, "type"),
                    };
                    let name = self.name(c);
                    self.push(k, label, name, c, depth, first);
                }
            }
            Sp::JsVar => {
                for c in self.children(id) {
                    if self.nodes.n[c].kind != "variable_declarator" {
                        continue;
                    }
                    let (k, label) =
                        match self.nodes.field(c, "value").map(|v| self.nodes.n[v].kind) {
                            Some(
                                "arrow_function"
                                | "function_expression"
                                | "function"
                                | "generator_function",
                            ) => (SymbolKind::Function, "function"),
                            Some("class") => (SymbolKind::Class, "class"),
                            _ => continue,
                        };
                    let name = self.name(c);
                    self.push(k, label, name, c, depth, first);
                }
            }
            Sp::CDecl => {
                // `typedef struct {...} X;`、`struct X {...} y;`：只有带定义体的才算符号
                let Some(t) = self.nodes.field(id, "type") else {
                    return;
                };
                let (k, label) = match self.nodes.n[t].kind {
                    "struct_specifier" => (SymbolKind::Struct, "struct"),
                    "class_specifier" => (SymbolKind::Class, "class"),
                    "enum_specifier" => (SymbolKind::Enum, "enum"),
                    _ => return,
                };
                if self.nodes.field(t, "body").is_none() {
                    return;
                }
                let mut name = self
                    .nodes
                    .field(t, "name")
                    .map_or(String::new(), |c| self.clean(self.text(c)));
                if name.is_empty() {
                    name = self
                        .nodes
                        .field(id, "declarator")
                        .map_or(String::new(), |c| self.clean(self.text(c)));
                }
                self.push(k, label, name, id, depth, first);
            }
        }
    }

    /// Elixir 里一切都是 `call`：按目标标识符判断是 `def*`、`defmodule` 还是导入。
    fn ex_call(&mut self, id: usize, depth: u32, first: &mut Option<String>) {
        let Some(t) = self.nodes.field(id, "target") else {
            return;
        };
        if self.nodes.n[t].kind != "identifier" {
            return;
        }
        let word = self.text(t).to_string();
        let kids = self.children(id);
        let args = kids
            .iter()
            .copied()
            .find(|&c| self.nodes.n[c].kind == "arguments");
        let first_arg = args.and_then(|a| {
            self.children(a)
                .into_iter()
                .find(|&c| self.nodes.n[c].kind != "comment")
        });
        let (kind, label, container) = match word.as_str() {
            "import" | "alias" | "use" | "require" => return self.import(id),
            "defmodule" => (SymbolKind::Module, "module", true),
            "defprotocol" => (SymbolKind::Interface, "protocol", true),
            "defimpl" => (SymbolKind::Module, "impl", true),
            "def" | "defp" | "defmacro" | "defmacrop" => {
                if depth > 0 {
                    (SymbolKind::Method, "method", false)
                } else {
                    (SymbolKind::Function, "function", false)
                }
            }
            _ => return,
        };
        // 函数头可能是 `name(args)`、`name(args) when guard` 或单独的标识符
        let mut at = first_arg;
        for _ in 0..8 {
            match at {
                Some(a) if self.nodes.n[a].kind == "binary_operator" => {
                    at = self.nodes.field(a, "left")
                }
                Some(a) if self.nodes.n[a].kind == "call" => at = self.nodes.field(a, "target"),
                _ => break,
            }
        }
        let name = at.map(|a| self.clean(self.text(a))).unwrap_or_default();
        self.push(kind, label, name, id, depth, first);
        if container {
            for c in kids
                .into_iter()
                .filter(|&c| self.nodes.n[c].kind == "do_block")
            {
                for d in self.children(c) {
                    self.walk(d, true, 1, depth + 1, first);
                }
            }
        }
    }

    fn go_imports(&mut self, id: usize, depth: u32) {
        for c in self.children(id) {
            match self.nodes.n[c].kind {
                "import_spec" => self.import(c),
                "import_spec_list" if depth < 2 => self.go_imports(c, depth + 1),
                _ => {}
            }
        }
    }
}

fn is_identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "simple_identifier"
            | "field_identifier"
            | "qualified_identifier"
            | "type_identifier"
            | "value_name"
            | "module_name"
            | "type_constructor"
            | "class_name"
            | "module_type_name"
            | "method_identifier"
            | "destructor_name"
            | "operator_name"
    )
}
