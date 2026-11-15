//! JSON value flattening helpers emitting pre-alignment [`RawFieldTrace`] records.

use std::collections::HashSet;

use serde_json::Value;

use crate::delta::DocumentInput;
use crate::evidence::{ExtractionMethod, RawFieldTrace};
use crate::hash::compact_excerpt;

/// Merge explicit traces with flattened JSON-LD and schema extraction traces for a document.
pub(crate) fn collect_all_traces(final_url: &str, input: &DocumentInput) -> Vec<RawFieldTrace> {
    let mut traces = input.raw_traces.clone();
    let mut seen: HashSet<(String, String)> = traces
        .iter()
        .map(|t| (t.field_path.clone(), t.raw_quote.clone()))
        .collect();
    let has_json_ld_traces = traces.iter().any(|t| {
        matches!(t.method, ExtractionMethod::JsonLd { .. }) || t.field_path.starts_with("/json-ld/")
    });
    let has_selector_traces = traces.iter().any(|t| {
        matches!(
            t.method,
            ExtractionMethod::CssSelector { .. }
                | ExtractionMethod::XPathSelector { .. }
                | ExtractionMethod::RegexPattern { .. }
        )
    });
    let mut add_extra = Vec::new();
    if !has_json_ld_traces {
        for (idx, item) in input.structured_json_ld.iter().enumerate() {
            let obj = item.as_object();
            let entity_type = obj
                .and_then(|m| m.get("@type"))
                .and_then(|v| {
                    v.as_str()
                        .or_else(|| v.as_array().and_then(|a| a.first()?.as_str()))
                })
                .map(str::to_owned);
            let entity_id = obj
                .and_then(|m| m.get("@id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let method = ExtractionMethod::JsonLd {
                entity_type,
                entity_id,
            };
            collect_json_traces(
                item,
                &format!("/json-ld/{idx}"),
                &method,
                false,
                Some(final_url),
                &mut add_extra,
            );
        }
    }
    if !has_selector_traces && let Some(val) = &input.extracted_fields {
        let method = ExtractionMethod::CssSelector {
            selector: "schema".into(),
            relocated_from: None,
            step: None,
        };
        collect_json_traces(val, "", &method, true, Some(final_url), &mut add_extra);
    }
    add_extra.retain(|t| t.field_name != "@context");
    for trace in add_extra {
        if seen.insert((trace.field_path.clone(), trace.raw_quote.clone())) {
            traces.push(trace);
        }
    }
    traces
}

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
