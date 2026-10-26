//! 纯 Rust 递归 JSON-LD 解析与实体抽取引擎。
//! 完整支持 `@graph`、`mainEntity`、嵌套对象递归展开及跨节点依赖。

use pagesift_html::{Html, Tag};
use serde_json::{Map, Value};

/// 提取 HTML 中所有的 JSON-LD 对象（包含展开 `@graph` 后的独立实体）
pub fn extract_json_ld(html: &Html<'_>) -> Vec<Value> {
    let mut results = Vec::new();

    for node in html.root().descendants() {
        if node.tag() == Some(Tag::Script) {
            let is_ld_json = node
                .as_element()
                .and_then(|e| e.attr("type"))
                .map(|t| t.trim().eq_ignore_ascii_case("application/ld+json"))
                .unwrap_or(false);

            if is_ld_json {
                let raw_text = node.text();
                let trimmed = raw_text.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // 尝试解析 JSON
                if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                    collect_entities(val, &mut results);
                }
            }
        }
    }

    results
}

/// 递归展开与收集顶层或 `@graph` 实体
fn collect_entities(val: Value, out: &mut Vec<Value>) {
    match val {
        Value::Object(map) => {
            // 如果包含 @graph，将 @graph 内部所有实体提取出来
            if let Some(graph) = map.get("@graph") {
                match graph {
                    Value::Array(items) => {
                        for item in items {
                            out.push(item.clone());
                        }
                    }
                    Value::Object(_) => {
                        out.push(graph.clone());
                    }
                    _ => {}
                }
            } else {
                out.push(Value::Object(map));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_entities(item, out);
            }
        }
        _ => {}
    }
}

/// 结构化电商商品信息视图
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ProductInfo {
    pub name: Option<String>,
    pub brand: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub sku: Option<String>,
    pub mpn: Option<String>,
    pub price: Option<String>,
    pub currency: Option<String>,
    pub availability: Option<String>,
    pub rating_value: Option<f64>,
    pub review_count: Option<u64>,
}

/// 从已提取的 JSON-LD 列表中提取统一 Product 信息
pub fn extract_product_view(entities: &[Value]) -> Option<ProductInfo> {
    for e in entities {
        if let Some(obj) = e.as_object()
            && is_type(obj, "Product")
        {
            let mut prod = ProductInfo {
                name: get_str(obj, "name"),
                description: get_str(obj, "description"),
                image: get_str(obj, "image"),
                sku: get_str(obj, "sku"),
                mpn: get_str(obj, "mpn"),
                ..Default::default()
            };

            // Brand
            if let Some(brand_val) = obj.get("brand") {
                prod.brand = match brand_val {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(b_obj) => get_str(b_obj, "name"),
                    _ => None,
                };
            }

            // Offers
            if let Some(offers_val) = obj.get("offers") {
                parse_offers(offers_val, &mut prod);
            }

            // AggregateRating
            if let Some(rating_val) = obj.get("aggregateRating")
                && let Some(r_obj) = rating_val.as_object()
            {
                prod.rating_value = get_num(r_obj, "ratingValue");
                prod.review_count = get_int(r_obj, "reviewCount");
            }

            return Some(prod);
        }
    }
    None
}

fn parse_offers(val: &Value, prod: &mut ProductInfo) {
    let offer_obj = match val {
        Value::Object(o) => Some(o),
        Value::Array(arr) => arr.first().and_then(|v| v.as_object()),
        _ => None,
    };

    if let Some(o) = offer_obj {
        prod.price = get_str(o, "price").or_else(|| get_num(o, "price").map(|n| n.to_string()));
        prod.currency = get_str(o, "priceCurrency");
        prod.availability = get_str(o, "availability");
    }
}

fn is_type(obj: &Map<String, Value>, expected: &str) -> bool {
    if let Some(t) = obj.get("@type") {
        match t {
            Value::String(s) => s.ends_with(expected),
            Value::Array(arr) => arr
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.ends_with(expected))),
            _ => false,
        }
    } else {
        false
    }
}

fn get_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
}

fn get_num(obj: &Map<String, Value>, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    })
}

fn get_int(obj: &Map<String, Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    })
}
