//! 结构树（Tagged PDF，ISO 32000-1 §14.7–14.8）：找出标题元素，记下它们的内容挂在哪一页的哪些 MCID 上。
//!
//! 只关心标题：`H1`–`H6` 层级直接给出；`H` 的层级按外层 `Sect` 的嵌套数算；`Title` 当一级。
//! 自定义类型先经 `/RoleMap` 映射到标准类型。

use std::collections::{HashMap, HashSet};

use crate::file::{Dictionary, Document, Object, ObjectId};
use crate::obj::deref;

/// 最多遍历这么多个结构节点（大文档的结构树可达几十万个节点）。
const MAX_NODES: usize = 2_000_000;
const MAX_ROLE_HOPS: usize = 8;

/// 每页：MCID → (标题层级 1–6, 标题元素的序号)。同一个元素的几行合成一个标题，不同元素分开。
pub(crate) type HeadingMcids = Vec<HashMap<i32, (u8, u32)>>;

struct Frame<'a> {
    node: &'a Object,
    page: Option<ObjectId>,
    heading: Option<(u8, u32)>,
    sects: u8,
}

/// 读结构树里的标题。没有结构树、或树里一个标题都没有时返回 None。
pub(crate) fn headings(doc: &Document, pages: &[ObjectId]) -> Option<HeadingMcids> {
    let catalog = match deref(doc, doc.trailer.get(b"Root")?) {
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let tree = match deref(doc, catalog.get(b"StructTreeRoot")?) {
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let role_map = match tree.get(b"RoleMap").map(|o| deref(doc, o)) {
        Some(Object::Dictionary(d)) => Some(d),
        _ => None,
    };
    let page_index: HashMap<ObjectId, usize> =
        pages.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let mut out: HeadingMcids = vec![HashMap::new(); pages.len()];
    let mut found = false;
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack = vec![Frame {
        node: tree.get(b"K")?,
        page: None,
        heading: None,
        sects: 0,
    }];
    let mut budget = MAX_NODES;
    let mut elements = 0u32;
    while let Some(f) = stack.pop() {
        // 超过截止时间就停：后面逐页循环里的检查会报超时
        if budget == 0 || (budget.is_multiple_of(4096) && crate::budget::expired()) {
            break;
        }
        budget -= 1;
        if let Object::Reference(id) = f.node
            && !seen.insert(*id)
        {
            continue;
        }
        match deref(doc, f.node) {
            Object::Array(items) => {
                // 倒序压栈，出栈时按文档顺序
                for item in items.iter().rev() {
                    stack.push(Frame { node: item, ..f });
                }
            }
            Object::Integer(mcid) => {
                if let (Some(heading), Some(page)) = (f.heading, f.page)
                    && let Some(&i) = page_index.get(&page)
                    && let Ok(mcid) = i32::try_from(*mcid)
                {
                    out[i].insert(mcid, heading);
                    found = true;
                }
            }
            Object::Dictionary(d) => {
                let page = match d.get(b"Pg") {
                    Some(Object::Reference(p)) => Some(*p),
                    _ => f.page,
                };
                match d.type_name() {
                    // 标记内容引用：MCID 可能带自己的 /Pg；表单里的（/Stm）不管
                    Some(b"MCR") => {
                        if d.get(b"Stm").is_none()
                            && let Some(mcid) = d.get(b"MCID")
                        {
                            stack.push(Frame {
                                node: mcid,
                                page,
                                ..f
                            });
                        }
                    }
                    Some(b"OBJR") => {}
                    _ => {
                        let role = d
                            .get(b"S")
                            .and_then(Object::as_name)
                            .map(|s| standard(doc, role_map, s));
                        let (heading, sects) = match role.as_deref() {
                            Some(b"Sect") => (f.heading, f.sects.saturating_add(1)),
                            Some(r) => match heading_level(r, f.sects) {
                                Some(level) => {
                                    elements += 1;
                                    (Some((level, elements)), f.sects)
                                }
                                None => (f.heading, f.sects),
                            },
                            None => (f.heading, f.sects),
                        };
                        if let Some(k) = d.get(b"K") {
                            stack.push(Frame {
                                node: k,
                                page,
                                heading,
                                sects,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    found.then_some(out)
}

/// 自定义结构类型经 /RoleMap 映射到标准类型（可能要映射好几次）。
fn standard(doc: &Document, role_map: Option<&Dictionary>, name: &[u8]) -> Vec<u8> {
    let mut cur = name.to_vec();
    let Some(map) = role_map else { return cur };
    for _ in 0..MAX_ROLE_HOPS {
        if is_standard(&cur) {
            break;
        }
        match map.get(&cur).map(|o| deref(doc, o)) {
            Some(Object::Name(n)) if *n != cur => cur = n.clone(),
            _ => break,
        }
    }
    cur
}

fn is_standard(name: &[u8]) -> bool {
    heading_level(name, 0).is_some()
        || matches!(
            name,
            b"Document"
                | b"Part"
                | b"Art"
                | b"Sect"
                | b"Div"
                | b"P"
                | b"L"
                | b"LI"
                | b"Lbl"
                | b"LBody"
                | b"Table"
                | b"TR"
                | b"TH"
                | b"TD"
                | b"Figure"
                | b"Caption"
                | b"Span"
                | b"Link"
                | b"TOC"
                | b"TOCI"
                | b"Note"
                | b"Reference"
                | b"Quote"
                | b"Code"
                | b"Formula"
        )
}

fn heading_level(role: &[u8], sects: u8) -> Option<u8> {
    match role {
        b"H1" => Some(1),
        b"H2" => Some(2),
        b"H3" => Some(3),
        b"H4" => Some(4),
        b"H5" => Some(5),
        b"H6" => Some(6),
        b"H" => Some(sects.clamp(1, 6)),
        b"Title" => Some(1),
        _ => None,
    }
}
