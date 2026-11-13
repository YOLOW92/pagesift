//! JSON value flattening helpers emitting pre-alignment [`RawFieldTrace`] records.

use serde_json::Value;

use crate::evidence::{ExtractionMethod, RawFieldTrace};
use crate::hash::compact_excerpt;

/// Flatten arbitrary JSON value leaves into `RawFieldTrace` records with a shared method builder.
pub fn collect_json_traces(
    val: &Value,
    pointer_prefix: &str,
    method: &ExtractionMethod,
    visible_in_dom: bool,
    source_url: Option<&str>,
    out: &mut Vec<RawFieldTrace>,
) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if k.starts_with('@') {
                    continue;
                }
                let escaped = k.replace('~', "~0").replace('/', "~1");
                let path = if pointer_prefix.is_empty() {
                    format!("/{escaped}")
                } else {
                    format!("{pointer_prefix}/{escaped}")
                };
                if matches!(v, Value::Object(_) | Value::Array(_)) {
                    collect_json_traces(v, &path, method, visible_in_dom, source_url, out);
                } else {
                    push_scalar_trace(v, &path, k, method, visible_in_dom, source_url, out);
                }
            }
        }
        Value::Array(arr) => {
            let parent_name = pointer_prefix.rsplit('/').next().unwrap_or("item");
            for (idx, item) in arr.iter().enumerate() {
                let path = format!("{pointer_prefix}/{idx}");
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        collect_json_traces(item, &path, method, visible_in_dom, source_url, out);
                    }
                    _ => push_scalar_trace(
                        item,
                        &path,
                        parent_name,
                        method,
                        visible_in_dom,
                        source_url,
                        out,
                    ),
                }
            }
        }
        _ => {
            let field_name = pointer_prefix.rsplit('/').next().unwrap_or("value");
            let path = if pointer_prefix.is_empty() {
                "/value".to_owned()
            } else {
                pointer_prefix.to_owned()
            };
            push_scalar_trace(
                val,
                &path,
                field_name,
                method,
                visible_in_dom,
                source_url,
                out,
            );
        }
    }
}

fn push_scalar_trace(
    val: &Value,
    field_path: &str,
    field_name: &str,
    method: &ExtractionMethod,
    visible_in_dom: bool,
    source_url: Option<&str>,
    out: &mut Vec<RawFieldTrace>,
) {
    if val.is_null() {
        return;
    }
    let quote = compact_excerpt(&scalar_quote(val), 240); // [策略] JSON 证据引用片段长度上限
    if !quote.is_empty() {
        out.push(RawFieldTrace {
            field_path: field_path.to_owned(),
            field_name: field_name.to_owned(),
            value: val.clone(),
            raw_quote: quote,
            attr_name: None,
            method: method.clone(),
            source_url: source_url.map(str::to_owned),
            visible_in_dom,
        });
    }
}

pub(crate) fn scalar_quote(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}
