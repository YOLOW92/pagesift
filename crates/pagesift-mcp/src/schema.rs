//! 工具参数校验：只实现工具 inputSchema 用到的 JSON Schema 子集。
//!
//! 支持 `type`、`properties`、`required`、`additionalProperties`、`items`、`enum`、`const`、
//! `minimum`/`maximum`、`minLength`/`maxLength`、`minItems`/`maxItems`、`format: "uri"`。
//! 其余关键字忽略（视为放行）。

use serde_json::{Map, Value};

const MAX_DEPTH: usize = 32;

/// 按 `schema` 校验 `value`，失败时返回带 JSON 路径的说明。
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    check(schema, value, "$", 0)
}

fn check(schema: &Value, value: &Value, path: &str, depth: usize) -> Result<(), String> {
    let Value::Object(s) = schema else {
        // `true` 或非对象 schema：放行；`false`：拒绝。
        return if *schema == Value::Bool(false) {
            Err(format!("{path}: no value is allowed here"))
        } else {
            Ok(())
        };
    };
    if depth > MAX_DEPTH {
        return Ok(());
    }

    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_matches(t, value),
            Value::Array(ts) => ts
                .iter()
                .filter_map(Value::as_str)
                .any(|t| type_matches(t, value)),
            _ => true,
        };
        if !ok {
            return Err(format!(
                "{path}: expected {}, got {}",
                type_label(t),
                kind(value)
            ));
        }
    }
    if let Some(c) = s.get("const")
        && c != value
    {
        return Err(format!("{path}: must equal {c}"));
    }
    if let Some(Value::Array(options)) = s.get("enum")
        && !options.contains(value)
    {
        let list: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(format!("{path}: must be one of {}", list.join(", ")));
    }

    match value {
        Value::Object(obj) => check_object(s, obj, path, depth),
        Value::Array(items) => {
            let len = items.len() as u64;
            if let Some(min) = s.get("minItems").and_then(Value::as_u64)
                && len < min
            {
                return Err(format!("{path}: needs at least {min} items"));
            }
            if let Some(max) = s.get("maxItems").and_then(Value::as_u64)
                && len > max
            {
                return Err(format!("{path}: allows at most {max} items"));
            }
            if let Some(item_schema) = s.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(item_schema, item, &format!("{path}[{i}]"), depth + 1)?;
                }
            }
            Ok(())
        }
        Value::String(text) => {
            let len = text.chars().count() as u64;
            if let Some(min) = s.get("minLength").and_then(Value::as_u64)
                && len < min
            {
                return Err(format!("{path}: must be at least {min} characters"));
            }
            if let Some(max) = s.get("maxLength").and_then(Value::as_u64)
                && len > max
            {
                return Err(format!("{path}: must be at most {max} characters"));
            }
            if s.get("format").and_then(Value::as_str) == Some("uri") && !looks_like_uri(text) {
                return Err(format!("{path}: must be an absolute URI"));
            }
            Ok(())
        }
        Value::Number(n) => {
            let Some(x) = n.as_f64() else { return Ok(()) };
            if let Some(min) = s.get("minimum").and_then(Value::as_f64)
                && x < min
            {
                return Err(format!("{path}: must be >= {min}"));
            }
            if let Some(max) = s.get("maximum").and_then(Value::as_f64)
                && x > max
            {
                return Err(format!("{path}: must be <= {max}"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn check_object(
    s: &Map<String, Value>,
    obj: &Map<String, Value>,
    path: &str,
    depth: usize,
) -> Result<(), String> {
    if let Some(Value::Array(req)) = s.get("required") {
        for key in req.iter().filter_map(Value::as_str) {
            if !obj.contains_key(key) {
                return Err(format!("{path}: missing required property \"{key}\""));
            }
        }
    }
    let props = s.get("properties").and_then(Value::as_object);
    for (key, v) in obj {
        let sub = format!("{path}.{key}");
        match props.and_then(|p| p.get(key)) {
            Some(prop_schema) => check(prop_schema, v, &sub, depth + 1)?,
            None => match s.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    return Err(format!("{path}: unknown property \"{key}\""));
                }
                Some(extra @ Value::Object(_)) => check(extra, v, &sub, depth + 1)?,
                _ => {}
            },
        }
    }
    Ok(())
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => match v {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        },
        _ => true,
    }
}

fn type_label(t: &Value) -> String {
    match t {
        Value::String(s) => s.clone(),
        Value::Array(ts) => ts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" or "),
        other => other.to_string(),
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// RFC 3986 的绝对 URI：`scheme ":" ...`，scheme 以字母开头。
fn looks_like_uri(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty()
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10 },
                "formats": { "type": "array", "items": { "enum": ["markdown", "html"] }, "maxItems": 2 },
                "opts": { "type": "object", "properties": { "x": { "type": "boolean" } }, "additionalProperties": false },
                "name": { "type": "string", "maxLength": 3 }
            },
            "required": ["url"]
        })
    }

    #[test]
    fn accepts_valid() {
        let v = json!({"url": "https://example.com", "limit": 3, "formats": ["html"], "opts": {"x": true}, "other": 1});
        validate(&schema(), &v).unwrap();
        validate(&schema(), &json!({"url": "http://a", "limit": 2.0})).unwrap();
    }

    #[test]
    fn rejects_invalid() {
        let s = schema();
        let bad = [
            (json!({}), "missing required property \"url\""),
            (json!({"url": 5}), "$.url: expected string, got number"),
            (json!({"url": "not a uri"}), "absolute URI"),
            (json!({"url": "http://a", "limit": 0}), ">= 1"),
            (json!({"url": "http://a", "limit": 1.5}), "expected integer"),
            (
                json!({"url": "http://a", "formats": ["pdf"]}),
                "$.formats[0]: must be one of",
            ),
            (
                json!({"url": "http://a", "formats": ["html", "html", "html"]}),
                "at most 2 items",
            ),
            (
                json!({"url": "http://a", "opts": {"y": 1}}),
                "unknown property \"y\"",
            ),
            (
                json!({"url": "http://a", "name": "日本語です"}),
                "at most 3 characters",
            ),
            (json!([1]), "expected object"),
        ];
        for (v, needle) in bad {
            let err = validate(&s, &v).unwrap_err();
            assert!(err.contains(needle), "{v}: {err}");
        }
    }

    #[test]
    fn deep_schema_is_bounded() {
        let mut s = json!({"type": "array"});
        for _ in 0..200 {
            s = json!({"type": "array", "items": s});
        }
        let mut v = json!([]);
        for _ in 0..50 {
            v = json!([v]);
        }
        validate(&s, &v).unwrap();
    }
}
