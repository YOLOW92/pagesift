//! JSON Schema 常用子集校验：type、properties、required、items、enum。

use serde_json::Value;

/// 校验通过返回 `Ok(())`，否则返回所有违例（带 JSON 路径）。
pub fn validate(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    check(schema, value, "$", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn type_matches(ty: &str, v: &Value) -> bool {
    match ty {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => {
            v.is_i64()
                || v.is_u64()
                || v.as_f64()
                    .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
        }
        _ => true,
    }
}

fn check(schema: &Value, v: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    match schema.get("type") {
        Some(Value::String(t)) => {
            if !type_matches(t, v) {
                errors.push(format!("{path}: 期望类型 {t}"));
                return;
            }
        }
        Some(Value::Array(ts))
            if !ts
                .iter()
                .filter_map(Value::as_str)
                .any(|t| type_matches(t, v)) =>
        {
            errors.push(format!("{path}: 类型不在允许列表内"));
            return;
        }
        _ => {}
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(v)
    {
        errors.push(format!("{path}: 不在 enum 允许的取值内"));
    }
    if let Some(obj) = v.as_object() {
        if let Some(Value::Array(req)) = schema.get("required") {
            for key in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(key) {
                    errors.push(format!("{path}: 缺少必填字段 {key}"));
                }
            }
        }
        if let Some(Value::Object(props)) = schema.get("properties") {
            for (key, sub) in props {
                if let Some(child) = obj.get(key) {
                    check(sub, child, &format!("{path}.{key}"), errors);
                }
            }
        }
    }
    if let (Some(arr), Some(items)) = (v.as_array(), schema.get("items")) {
        for (i, child) in arr.iter().enumerate() {
            check(items, child, &format!("{path}[{i}]"), errors);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_valid() {
        let s = json!({"type":"object","required":["a"],"properties":{
            "a":{"type":"integer"},"b":{"type":"array","items":{"type":"string","enum":["x","y"]}}}});
        assert!(validate(&s, &json!({"a":1,"b":["x","y"]})).is_ok());
    }

    #[test]
    fn reports_all_violations() {
        let s = json!({"type":"object","required":["a","c"],"properties":{
            "a":{"type":"integer"},"b":{"type":"array","items":{"enum":["x"]}}}});
        let errs = validate(&s, &json!({"a":"no","b":["z"]})).unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn integer_vs_number() {
        let s = json!({"type":"integer"});
        assert!(validate(&s, &json!(2.0)).is_ok());
        assert!(validate(&s, &json!(2.5)).is_err());
    }
}
