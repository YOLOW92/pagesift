//! 纯 Rust HTML5 Microdata 递归提取器，遵循 W3C Microdata 标准。
//! 提取 `itemscope`、`itemtype`、`itemid` 及嵌套的 `itemprop` 键值树。

use pagesift_html::{Html, NodeRef, Tag};
use serde_json::{Map, Value};

fn get_attr<'a>(node: NodeRef<'a>, name: &str) -> Option<&'a str> {
    node.as_element()?.attr(name)
}

/// 提取 HTML 中所有顶层 Microdata 实体
pub fn extract_microdata(html: &Html<'_>) -> Vec<Value> {
    let mut items = Vec::new();
    let root = html.root();

    // 寻找所有顶层 itemscope（即其父祖节点中没有其他包含它的未结 itemscope）
    for node in root.descendants() {
        if get_attr(node, "itemscope").is_some() && is_top_level_scope(node) {
            let item = parse_scope(node);
            items.push(Value::Object(item));
        }
    }

    items
}

/// 判断一个节点是否是顶层 scope（祖先没有非 itemprop 的 itemscope）
fn is_top_level_scope(node: NodeRef<'_>) -> bool {
    let mut curr = node.parent();
    while let Some(p) = curr {
        if get_attr(p, "itemscope").is_some() {
            // 如果祖先是另一个 scope，且当前节点是一个 itemprop 属性，那当前是嵌套项不是顶层
            return false;
        }
        curr = p.parent();
    }
    true
}

/// 解析一个具体的 scope
fn parse_scope(scope_node: NodeRef<'_>) -> Map<String, Value> {
    let mut item = Map::new();

    if let Some(itemtype) = get_attr(scope_node, "itemtype") {
        item.insert(
            "type".to_string(),
            Value::String(itemtype.trim().to_string()),
        );
    }
    if let Some(itemid) = get_attr(scope_node, "itemid") {
        item.insert("id".to_string(), Value::String(itemid.trim().to_string()));
    }

    let mut properties = Map::new();
    collect_properties(scope_node, scope_node, &mut properties);

    item.insert("properties".to_string(), Value::Object(properties));
    item
}

/// 递归收集当前 scope 的直接属性（跨越非 itemscope 子节点，遇到嵌套 itemscope 时作为嵌套项解析）
fn collect_properties(
    current: NodeRef<'_>,
    scope_root: NodeRef<'_>,
    props: &mut Map<String, Value>,
) {
    for child in current.children() {
        if let Some(itemprop_str) = get_attr(child, "itemprop") {
            let val = if get_attr(child, "itemscope").is_some() {
                // 当前属性元素本身开启了一个新的子 scope
                Value::Object(parse_scope(child))
            } else {
                extract_property_value(child)
            };

            // 支持空格分隔的多个属性名: itemprop="name title"
            for prop_name in itemprop_str.split_whitespace() {
                add_property(props, prop_name, val.clone());
            }

            // 如果当前属性元素开了新的 scope，其子元素属于子 scope，不可再归入本层
            if get_attr(child, "itemscope").is_some() {
                continue;
            }
        } else if get_attr(child, "itemscope").is_some() {
            // 无 itemprop 但有 itemscope 的子节点属于独立的子孤立 scope，不向本层蔓延
            continue;
        }

        // 继续向下递归寻找属于本层的 itemprop
        collect_properties(child, scope_root, props);
    }
}

/// 根据元素标签与规范提取属性值
fn extract_property_value(node: NodeRef<'_>) -> Value {
    let tag = node.tag();

    if tag == Some(Tag::Meta) {
        if let Some(content) = get_attr(node, "content") {
            return Value::String(content.to_string());
        }
    }

    if matches!(tag, Some(Tag::A | Tag::Area | Tag::Link)) {
        if let Some(href) = get_attr(node, "href") {
            return Value::String(href.to_string());
        }
    }

    if matches!(
        tag,
        Some(Tag::Img | Tag::Audio | Tag::Video | Tag::Source | Tag::Track | Tag::Embed)
    ) {
        if let Some(src) = get_attr(node, "src") {
            return Value::String(src.to_string());
        }
    }

    if tag == Some(Tag::Time) {
        if let Some(datetime) = get_attr(node, "datetime") {
            return Value::String(datetime.to_string());
        }
    }

    if matches!(tag, Some(Tag::Data | Tag::Meter)) {
        if let Some(val) = get_attr(node, "value") {
            return Value::String(val.to_string());
        }
    }

    // 默认取纯文本并去除首尾多余空白
    let text = node.text();
    Value::String(text.trim().to_string())
}


/// 将属性值放入属性字典，若同名属性出现多次，聚合为 Array
fn add_property(props: &mut Map<String, Value>, name: &str, val: Value) {
    match props.get_mut(name) {
        Some(existing) => match existing {
            Value::Array(arr) => {
                arr.push(val);
            }
            _ => {
                let prev = existing.take();
                *existing = Value::Array(vec![prev, val]);
            }
        },
        None => {
            props.insert(name.to_string(), val);
        }
    }
}
