//! 书签（文档大纲，ISO 32000-1 §12.3.3）：标题文字、层级、目标页和目标位置。
//!
//! 目标可以直接写数组，也可以经 `/A`（GoTo 动作）、旧式的 Catalog `/Dests` 字典、
//! 或 `/Names` 的 `/Dests` 名称树按名字查到。

use std::collections::{HashMap, HashSet};

use crate::file::{Dictionary, Document, Object, ObjectId};
use crate::obj::{deref, num, text_string};

const MAX_ITEMS: usize = 20_000;
const MAX_DEPTH: usize = 16;
/// 名称树最多查这么多个节点。
const MAX_NAME_NODES: usize = 100_000;

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) title: String,
    /// 从 1 开始。
    pub(crate) level: u8,
    /// 目标页的序号（从 0 开始）；解不出来为 None。
    pub(crate) page: Option<usize>,
    /// 目标位置的 y（PDF 用户空间，向上为正）；目标只给了页时为 None。
    pub(crate) top: Option<f64>,
}

/// 读整棵书签树，按文档顺序。没有书签返回空。
pub(crate) fn entries(doc: &Document, pages: &[ObjectId]) -> Vec<Entry> {
    let mut out = Vec::new();
    let Some(catalog) = catalog(doc) else {
        return out;
    };
    let Some(Object::Dictionary(root)) = catalog.get(b"Outlines").map(|o| deref(doc, o)) else {
        return out;
    };
    let page_index: HashMap<ObjectId, usize> = pages.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let mut seen = HashSet::new();
    // (节点, 层级)；兄弟和子节点都用显式栈，不递归
    let mut stack: Vec<(&Object, u8)> = Vec::new();
    if let Some(first) = root.get(b"First") {
        stack.push((first, 1));
    }
    while let Some((node, level)) = stack.pop() {
        if out.len() >= MAX_ITEMS {
            break;
        }
        if let Object::Reference(id) = node
            && !seen.insert(*id)
        {
            continue;
        }
        let Object::Dictionary(item) = deref(doc, node) else {
            continue;
        };
        // 先压兄弟，再压孩子：孩子先出栈，得到文档顺序
        if let Some(next) = item.get(b"Next") {
            stack.push((next, level));
        }
        if usize::from(level) < MAX_DEPTH
            && let Some(first) = item.get(b"First")
        {
            stack.push((first, level + 1));
        }
        let title = match item.get(b"Title").map(|o| deref(doc, o)) {
            Some(Object::String(s, _)) => text_string(s).split_whitespace().collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        let dest = item.get(b"Dest").map(|o| deref(doc, o)).or_else(|| {
            match item.get(b"A").map(|o| deref(doc, o)) {
                Some(Object::Dictionary(a)) if a.get(b"S").and_then(Object::as_name) == Some(b"GoTo") => {
                    a.get(b"D").map(|o| deref(doc, o))
                }
                _ => None,
            }
        });
        let (page, top) = dest
            .and_then(|d| resolve_dest(doc, catalog, d))
            .map(|arr| target(doc, arr, &page_index))
            .unwrap_or((None, None));
        out.push(Entry { title, level, page, top });
    }
    out
}

fn catalog<'a>(doc: &'a Document) -> Option<&'a Dictionary> {
    match deref(doc, doc.trailer.get(b"Root")?) {
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

/// 目标解析成数组：名字（Name 或 String）先查表；值可能是 `<< /D [...] >>`。
fn resolve_dest<'a>(doc: &'a Document, catalog: &'a Dictionary, dest: &'a Object) -> Option<&'a [Object]> {
    let found: &Object = match dest {
        Object::Array(a) => return Some(a),
        Object::Name(n) => match catalog.get(b"Dests").map(|o| deref(doc, o)) {
            Some(Object::Dictionary(d)) => deref(doc, d.get(n)?),
            _ => return None,
        },
        Object::String(s, _) => name_tree_lookup(doc, catalog, s)?,
        _ => return None,
    };
    match found {
        Object::Array(a) => Some(a),
        Object::Dictionary(d) => match deref(doc, d.get(b"D")?) {
            Object::Array(a) => Some(a),
            _ => None,
        },
        _ => None,
    }
}

/// `/Names /Dests` 名称树：按 /Limits 剪枝往下找。
fn name_tree_lookup<'a>(doc: &'a Document, catalog: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let names = match deref(doc, catalog.get(b"Names")?) {
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let mut stack = vec![names.get(b"Dests")?];
    let mut budget = MAX_NAME_NODES;
    while let Some(node) = stack.pop() {
        if budget == 0 {
            return None;
        }
        budget -= 1;
        let Object::Dictionary(n) = deref(doc, node) else {
            continue;
        };
        if let Some(Object::Array(lim)) = n.get(b"Limits").map(|o| deref(doc, o))
            && let (Some(Object::String(lo, _)), Some(Object::String(hi, _))) =
                (lim.first().map(|o| deref(doc, o)), lim.get(1).map(|o| deref(doc, o)))
            && (key < lo.as_slice() || key > hi.as_slice())
        {
            continue;
        }
        if let Some(Object::Array(pairs)) = n.get(b"Names").map(|o| deref(doc, o)) {
            for pair in pairs.chunks(2) {
                if let [k, v] = pair
                    && let Object::String(s, _) = deref(doc, k)
                    && s.as_slice() == key
                {
                    return Some(deref(doc, v));
                }
            }
        }
        if let Some(Object::Array(kids)) = n.get(b"Kids").map(|o| deref(doc, o)) {
            stack.extend(kids.iter());
        }
    }
    None
}

/// 目标数组 `[页 /XYZ left top zoom]`、`[页 /FitH top]`、`[页 /FitR l b r t]` ……
fn target(doc: &Document, arr: &[Object], page_index: &HashMap<ObjectId, usize>) -> (Option<usize>, Option<f64>) {
    let page = match arr.first() {
        Some(Object::Reference(id)) => page_index.get(id).copied(),
        // 远程目标或写错的文件用页序号
        Some(Object::Integer(i)) => usize::try_from(*i).ok().filter(|&i| i < page_index.len()),
        _ => None,
    };
    let at = |i: usize| arr.get(i).map(|o| deref(doc, o)).and_then(num);
    let top = match arr.get(1).and_then(Object::as_name) {
        Some(b"XYZ") => at(3),
        Some(b"FitH" | b"FitBH") => at(2),
        Some(b"FitR") => at(5),
        _ => None,
    };
    (page, top)
}
