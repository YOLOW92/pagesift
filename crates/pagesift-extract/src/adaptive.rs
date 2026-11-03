//! 带记忆的结构化抽取：选择器失效时用 [`crate::locate`] 重定位，生成稳健的新选择器写回记忆。
//!
//! 记忆的键是 `baseSelector` 和 `fields.<名字>`（顶层字段，相对列表项）；存储（注册域、schema id）
//! 由调用方的 [`SelectorMemory`] 实现决定。精确命中时刷新记忆里的指纹；重定位成功时写回新选择器，
//! 结果里列出新旧选择器（调用方报 `relocated` 警告）；找不到或有歧义时如实列出，不猜。

use std::cell::RefCell;
use std::collections::HashMap;

use pagesift_html::{Html, NodeRef, Selector};
use serde::Serialize;
use serde_json::Value;

use crate::locate::{Located, Target, find_similar, locate_each, locate_list, robust_selector};
use crate::{Error, FieldKind, JsonCssExtractor, Schema};

/// 选择器记忆：按字段键读写 [`Target`]。
pub trait SelectorMemory {
    fn recall(&self, field: &str) -> Option<Target>;
    fn remember(&self, field: &str, target: &Target);
}

/// 不记忆：只做精确抽取（和 [`JsonCssExtractor`] 一样）。
pub struct NoMemory;

impl SelectorMemory for NoMemory {
    fn recall(&self, _: &str) -> Option<Target> {
        None
    }
    fn remember(&self, _: &str, _: &Target) {}
}

/// 进程内的记忆（测试、一次性批量用）。
#[derive(Default)]
pub struct InMemory(RefCell<HashMap<String, Target>>);

impl SelectorMemory for InMemory {
    fn recall(&self, field: &str) -> Option<Target> {
        self.0.borrow().get(field).cloned()
    }
    fn remember(&self, field: &str, target: &Target) {
        self.0
            .borrow_mut()
            .insert(field.to_string(), target.clone());
    }
}

/// 一次重定位：哪个字段、旧选择器、新选择器、走到了哪一步（`anchor` 或 `fingerprint`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldRelocation {
    pub field: String,
    pub old: String,
    pub new: String,
    pub step: &'static str,
}

/// 抽取结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AdaptiveOutcome {
    pub items: Vec<Value>,
    pub relocated: Vec<FieldRelocation>,
    /// 选择器失效、记忆也没能找回的字段：`(字段, "missing" | "ambiguous")`。
    pub unresolved: Vec<(String, &'static str)>,
}

impl AdaptiveOutcome {
    /// 对外的警告：`relocated: <字段> <旧> -> <新>`、`extract_missing: <字段>`、`extract_ambiguous: <字段>`。
    pub fn warnings(&self) -> Vec<String> {
        let mut w: Vec<String> = self
            .relocated
            .iter()
            .map(|r| format!("relocated: {} {} -> {}", r.field, r.old, r.new))
            .collect();
        w.extend(
            self.unresolved
                .iter()
                .map(|(f, why)| format!("extract_{why}: {f}")),
        );
        w
    }
}

const BASE: &str = "baseSelector";

fn css_relative(sel: &str) -> String {
    if sel.trim_start().starts_with('>') {
        format!(":scope {}", sel.trim())
    } else {
        sel.trim().to_string()
    }
}

/// 按 schema 抽取，选择器失效时借助 `memory` 重定位。
pub fn extract_adaptive(
    doc: &Html<'_>,
    schema: &Schema,
    memory: &dyn SelectorMemory,
) -> Result<AdaptiveOutcome, Error> {
    let mut patched = schema.clone();
    let mut out = AdaptiveOutcome::default();
    let root = doc.root();

    // 列表项
    let base_sel = Selector::parse(&schema.base_selector).map_err(|e| Error::Selector {
        path: BASE.into(),
        selector: schema.base_selector.clone(),
        message: e.to_string(),
    })?;
    let first = doc.select(&base_sel).next();
    match first {
        Some(n) => refresh(memory, BASE, n, &schema.base_selector, None),
        None => match memory.recall(BASE) {
            Some(t) => match relocate_base(root, &t) {
                Ok((node, new, step)) => {
                    out.relocated.push(FieldRelocation {
                        field: BASE.into(),
                        old: schema.base_selector.clone(),
                        new: new.clone(),
                        step,
                    });
                    refresh(memory, BASE, node, &new, None);
                    patched.base_selector = new;
                }
                Err(why) => out.unresolved.push((BASE.into(), why)),
            },
            None => out.unresolved.push((BASE.into(), "missing")),
        },
    }

    // 顶层字段（相对列表项）
    let items: Vec<NodeRef<'_>> = match Selector::parse(&patched.base_selector) {
        Ok(s) => doc.select(&s).collect(),
        Err(_) => Vec::new(),
    };
    // 列表项都没找到：字段没有作用域，报列表项一条就够
    if items.is_empty() {
        return Ok(out);
    }
    for (i, field) in schema.fields.iter().enumerate() {
        let (Some(sel_text), None) = (&field.selector, &field.source) else {
            continue;
        };
        if matches!(field.kind, FieldKind::Computed) {
            continue;
        }
        let key = format!("fields.{}", field.name);
        let Ok(sel) = Selector::parse(&css_relative(sel_text)) else {
            continue;
        };
        let hit = items.iter().find_map(|it| it.select(&sel).next());
        if let Some(n) = hit {
            refresh(memory, &key, n, sel_text, field.identity.as_deref());
            continue;
        }
        let Some(t) = memory.recall(&key) else {
            out.unresolved.push((key, "missing"));
            continue;
        };
        let mut why = "missing";
        let mut fixed = None;
        for (it, located) in items.iter().zip(locate_each(&items, &t)) {
            let step = match located {
                Located::Exact(_) => "exact",
                Located::Anchored(_) => "anchor",
                Located::Relocated { .. } => "fingerprint",
                Located::Ambiguous { .. } => {
                    why = "ambiguous";
                    continue;
                }
                Located::Missing => continue,
            };
            if let Some(n) = located.node() {
                fixed = Some((n, *it, step));
                break;
            }
        }
        let fixed = fixed.and_then(|(n, scope, step)| {
            let sel = if step == "exact" {
                Some(t.selector.clone())
            } else {
                robust_selector(n, scope)
            };
            sel.map(|s| (n, s, step))
        });
        match fixed {
            Some((n, new, step)) => {
                out.relocated.push(FieldRelocation {
                    field: key.clone(),
                    old: sel_text.clone(),
                    new: new.clone(),
                    step,
                });
                refresh(memory, &key, n, &new, field.identity.as_deref());
                patched.fields[i].selector = Some(new);
            }
            None => out.unresolved.push((key, why)),
        }
    }

    out.items = JsonCssExtractor::new(&patched)?.extract_document(doc);
    Ok(out)
}

/// 记下命中的元素。配了身份属性而元素上没有时不记（没有身份的记忆会退回相似度去猜）。
fn refresh(
    memory: &dyn SelectorMemory,
    key: &str,
    node: NodeRef<'_>,
    selector: &str,
    identity: Option<&str>,
) {
    if let Ok(t) = Target::from_node(node, selector, identity) {
        memory.remember(key, &t);
    }
}

/// 列表项失效：找回记忆里的那一项，再用 `find_similar` 拼出取整个列表的选择器。
fn relocate_base<'a>(
    root: NodeRef<'a>,
    t: &Target,
) -> Result<(NodeRef<'a>, String, &'static str), &'static str> {
    let located = locate_list(root, t);
    let step = match &located {
        Located::Anchored(_) => "anchor",
        Located::Exact(_) => "exact",
        Located::Relocated { .. } => "fingerprint",
        Located::Ambiguous { .. } => return Err("ambiguous"),
        Located::Missing => return Err("missing"),
    };
    let node = located.node().ok_or("missing")?;
    if step == "exact" {
        // 记忆里的当前选择器（上次重定位写回的）照旧命中
        return Ok((node, t.selector.clone(), step));
    }
    let sel = find_similar(node, root)
        .filter(|s| s.depth == 0)
        .and_then(|s| s.selector)
        .or_else(|| robust_selector(node, root))
        .ok_or("missing")?;
    Ok((node, sel, step))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn relocates_list_and_fields_after_redesign() {
        let schema = Schema::from_json(&json!({
            "baseSelector": "li.product",
            "fields": [
                {"name": "title", "selector": "h3.name", "type": "text"},
                {"name": "price", "selector": "span.price", "type": "text"}
            ]
        }))
        .unwrap();
        let mem = InMemory::default();
        let names = [
            "Alpha widget",
            "Beta gadget",
            "Gamma tool",
            "Delta kit",
            "Epsilon set",
        ];
        let chrome = |body: String| {
            let nav: String = (0..12)
                .map(|i| format!("<a href='/c{i}'>Category {i}</a>"))
                .collect();
            let foot: String = (0..12)
                .map(|i| format!("<p>Footer note number {i} about shipping</p>"))
                .collect();
            format!("<header><nav>{nav}</nav></header><main>{body}</main><footer>{foot}</footer>")
        };
        let items1: String = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                format!("<li class='product'><h3 class='name'>{n}</h3><span class='price'>${i}</span></li>")
            })
            .collect();
        let v1_src = chrome(format!("<ul>{items1}</ul>"));
        let v1 = Html::parse(&v1_src);
        let first = extract_adaptive(&v1, &schema, &mem).unwrap();
        assert_eq!(first.items.len(), 5);
        assert!(first.relocated.is_empty());
        // 改版：类名哈希化、列表项换成 div、字段换标签
        let items2: String = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                format!("<div class='css-1x8dfa'><h4 class='sc-a3b9'>{n}</h4><b class='sc-q9z2'>${i}</b></div>")
            })
            .collect();
        let v2_src = chrome(format!("<div class='grid'>{items2}</div>"));
        let v2 = Html::parse(&v2_src);
        let second = extract_adaptive(&v2, &schema, &mem).unwrap();
        assert_eq!(second.items.len(), 5, "{second:?}");
        assert_eq!(second.items[1]["title"], "Beta gadget");
        assert_eq!(second.items[1]["price"], "$1");
        assert!(
            second
                .warnings()
                .iter()
                .any(|w| w.starts_with("relocated: baseSelector"))
        );
        // 第三次：记忆里已是新选择器，精确命中
        let third = extract_adaptive(&v2, &schema, &mem).unwrap();
        assert_eq!(third.items, second.items);
    }

    fn single(field: serde_json::Value) -> Schema {
        Schema::from_json(&json!({"baseSelector": "main", "fields": [field]})).unwrap()
    }

    #[test]
    fn missing_is_reported_without_memory() {
        let doc = Html::parse("<main><p>x</p></main>");
        let f = extract_adaptive(
            &doc,
            &single(json!({"name": "t", "selector": "h1", "type": "text"})),
            &NoMemory,
        )
        .unwrap();
        assert_eq!(f.warnings(), ["extract_missing: fields.t"]);
        let s = Schema::from_json(&json!({"baseSelector": "ul", "fields": [
            {"name": "t", "selector": "h1", "type": "text"}]}))
        .unwrap();
        let b = extract_adaptive(&doc, &s, &NoMemory).unwrap();
        assert_eq!(b.warnings(), ["extract_missing: baseSelector"]);
    }

    #[test]
    fn identity_reaches_the_anchor_step_and_never_guesses() {
        let schema = single(
            json!({"name": "sku", "selector": "span.sku", "type": "text", "identity": "data-sku"}),
        );
        let remembered = || {
            let mem = InMemory::default();
            let v1 =
                Html::parse("<main><span class='sku' data-sku='A1'>one</span><p>other</p></main>");
            let first = extract_adaptive(&v1, &schema, &mem).unwrap();
            assert!(first.warnings().is_empty());
            mem
        };
        // 改版：类名、标签都变了，身份还在
        let v2 = Html::parse("<main><b class='x' data-sku='A1'>one</b><p>other</p></main>");
        let moved = extract_adaptive(&v2, &schema, &remembered()).unwrap();
        assert_eq!(moved.relocated[0].step, "anchor", "{moved:?}");
        assert_eq!(moved.items[0]["sku"], "one");
        // 身份重复：不猜
        let dup = Html::parse("<main><b data-sku='A1'>one</b><b data-sku='A1'>two</b></main>");
        let amb = extract_adaptive(&dup, &schema, &remembered()).unwrap();
        assert_eq!(amb.warnings(), ["extract_ambiguous: fields.sku"]);
        // 身份消失：报 missing，也不退回相似度
        let gone = Html::parse("<main><b data-sku='Z9'>one</b></main>");
        let miss = extract_adaptive(&gone, &schema, &remembered()).unwrap();
        assert_eq!(miss.warnings(), ["extract_missing: fields.sku"]);
    }
}
