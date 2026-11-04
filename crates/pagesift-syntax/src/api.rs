//! 对外接口：解析入口、节点遍历、S 表达式输出。

use crate::Limits;
use crate::parser::Parser;
use crate::table::{Language, SYM_ERROR};
use crate::tree::{Arena, Len, NONE};

/// 解析结果：树本身放在竞技场里，用 [`ParseTree::visit`] 遍历。
pub struct ParseTree {
    pub arena: Arena,
    pub root: u32,
    /// 因资源上限而中止，树是退化的单个 ERROR 节点。
    pub truncated: bool,
    pub input_len: usize,
}

/// 遍历事件里的可见节点信息。
pub struct VNode<'a> {
    pub kind: &'a str,
    pub named: bool,
    pub field: Option<&'a str>,
    pub start: usize,
    pub end: usize,
    pub missing: bool,
    pub error: bool,
    /// 词法层面跳过的非法字符（仅 ERROR 叶子），没有则为 0。
    pub unexpected: u32,
    pub has_children: bool,
}

pub enum Ev<'a> {
    Open(VNode<'a>),
    Close,
}

/// 解析一段字节输入。输入不可信：任何资源上限被触发都返回退化的树而不是崩溃。
pub fn parse(lang: &Language, input: &[u8], limits: Limits) -> ParseTree {
    let mut p = Parser::new(lang, input, limits);
    if input.len() >= u32::MAX as usize / 2 {
        p.aborted = true;
    } else {
        p.run();
    }
    let (mut root, mut truncated) = (p.finished, false);
    if root == NONE || p.aborted {
        // 退化：整段输入作为一个 ERROR 叶子
        let size = Len {
            bytes: input.len() as u32,
            rows: input.iter().filter(|b| **b == b'\n').count() as u32,
        };
        root = p.a.new_leaf(lang, SYM_ERROR, Len::default(), size);
        p.a.n_mut(root).error_cost = 1;
        truncated = true;
    }
    ParseTree {
        arena: p.a,
        root,
        truncated,
        input_len: input.len(),
    }
}

struct Frame {
    id: u32,
    offset: u32,
    alias: u16,
    alias_named: bool,
    field: Option<u16>,
    pre: bool,
    visible: bool,
    child_index: usize,
    structural: usize,
    next_offset: u32,
}

impl ParseTree {
    pub fn has_error(&self) -> bool {
        self.arena.n(self.root).cost() > 0
    }

    /// 官方 `ts_subtree__write_to_string` 的遍历：对每个可见节点发出 Open/Close。
    pub fn visit<'a>(&self, lang: &'a Language, mut f: impl FnMut(Ev<'a>)) {
        let a = &self.arena;
        let mut stack = vec![Frame {
            id: self.root,
            offset: 0,
            alias: 0,
            alias_named: false,
            field: None,
            pre: false,
            visible: false,
            child_index: 0,
            structural: 0,
            next_offset: 0,
        }];
        while let Some(top) = stack.len().checked_sub(1) {
            let id = stack[top].id;
            let node = a.n(id);
            if !stack[top].pre {
                let fr = &stack[top];
                let visible = node.missing()
                    || if fr.alias != 0 {
                        fr.alias_named
                    } else {
                        node.visible() && node.named()
                    };
                let sym = if fr.alias != 0 { fr.alias } else { node.sym };
                let start = fr.offset + node.padding.bytes;
                if visible {
                    f(Ev::Open(VNode {
                        kind: lang.symbol_name(sym),
                        named: fr.alias_named || node.named(),
                        field: fr.field.map(|x| lang.field_name(x)),
                        start: start as usize,
                        end: (start + node.size.bytes) as usize,
                        missing: node.missing(),
                        error: node.is_error(),
                        unexpected: if node.is_error()
                            && node.child_count == 0
                            && node.size.bytes > 0
                        {
                            node.lchar
                        } else {
                            0
                        },
                        has_children: node.child_count > 0,
                    }));
                }
                let fr = &mut stack[top];
                fr.visible = visible;
                fr.pre = true;
                fr.next_offset = fr.offset;
            }
            let fr_visible = stack[top].visible;
            if stack[top].child_index < node.child_count as usize {
                let ci = stack[top].child_index;
                let child = a.children(id)[ci];
                let cn = a.n(child);
                let mut cf = Frame {
                    id: child,
                    offset: stack[top].next_offset,
                    alias: 0,
                    alias_named: false,
                    field: None,
                    pre: false,
                    visible: false,
                    child_index: 0,
                    structural: 0,
                    next_offset: 0,
                };
                if !cn.extra() {
                    let seq = lang.alias_sequence(node.prod);
                    let al = seq.map_or(0, |s| s.get(stack[top].structural).copied().unwrap_or(0));
                    cf.alias = al;
                    cf.alias_named = al != 0 && lang.named(al);
                    let mut field = if fr_visible { None } else { stack[top].field };
                    if let Some(&(s, l)) = lang.field_map_slices.get(node.prod as usize) {
                        for e in lang
                            .field_map_entries
                            .iter()
                            .skip(s as usize)
                            .take(l as usize)
                        {
                            if !e.2 && e.1 as usize == stack[top].structural {
                                field = Some(e.0);
                                break;
                            }
                        }
                    }
                    cf.field = field;
                    stack[top].structural += 1;
                }
                stack[top].next_offset += cn.total().bytes;
                stack[top].child_index += 1;
                stack.push(cf);
                continue;
            }
            if fr_visible {
                f(Ev::Close);
            }
            stack.pop();
        }
    }

    /// 与 `tree-sitter` 的 `Node::to_sexp` 一致的 S 表达式（含字段名）。
    pub fn to_sexp(&self, lang: &Language) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        let mut first = true;
        // 根节点不可见时官方输出 "(name" 开头，这里同样处理
        let root = self.arena.n(self.root);
        let root_visible = root.missing() || (root.visible() && root.named());
        if !root_visible {
            out.push('(');
            out.push_str(lang.symbol_name(root.sym));
            depth += 1;
            first = false;
        }
        self.visit(lang, |ev| match ev {
            Ev::Open(n) => {
                if !first {
                    out.push(' ');
                    if let Some(fl) = n.field {
                        out.push_str(fl);
                        out.push_str(": ");
                    }
                }
                first = false;
                if n.unexpected != 0 || (n.error && !n.has_children && n.end > n.start) {
                    out.push_str("(UNEXPECTED ");
                    out.push_str(&char_repr(n.unexpected));
                } else if n.missing {
                    out.push_str("(MISSING ");
                    if n.named {
                        out.push_str(n.kind);
                    } else {
                        out.push('"');
                        out.push_str(n.kind);
                        out.push('"');
                    }
                } else {
                    out.push('(');
                    out.push_str(n.kind);
                }
                depth += 1;
            }
            Ev::Close => {
                out.push(')');
                depth -= 1;
            }
        });
        while depth > 0 {
            out.push(')');
            depth -= 1;
        }
        out
    }
}

fn char_repr(c: u32) -> String {
    match c {
        0 => "'\\0'".into(),
        10 => "'\\n'".into(),
        9 => "'\\t'".into(),
        13 => "'\\r'".into(),
        32..=126 => format!("'{}'", c as u8 as char),
        _ => c.to_string(),
    }
}
