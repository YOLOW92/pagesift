//! Crawl4AI 风格的提取 schema：解析、校验。选择器在各引擎里编译。

use std::str::FromStr;

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use crate::Error;

/// 提取 schema，字段名和 Crawl4AI 一致（`baseSelector`、`baseFields`、`fields`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    pub base_selector: String,
    /// 直接从 base 元素本身取的字段（通常是属性）。
    pub base_fields: Vec<Field>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    /// 相对当前元素的选择器；没有时就取当前元素本身。
    pub selector: Option<String>,
    /// Crawl4AI 的 `source`：`"+ tr.subtext"` 表示先跳到后面第一个匹配的兄弟元素。
    pub source: Option<String>,
    pub kind: FieldKind,
    /// 取不到值时的默认值（任意 JSON）。
    pub default: Option<Value>,
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// `text`、`attribute`、`html`、`regex`，或者它们组成的数组（按顺序串起来）。
    Value(Vec<Step>),
    /// 取第一个匹配元素，返回对象；没匹配到返回 `{}`。
    Nested(Vec<Field>),
    /// 每个匹配元素一个对象。
    List(Vec<Field>),
    /// 同 `List`，Crawl4AI 里两者只在内部实现上有区别。
    NestedList(Vec<Field>),
    /// Crawl4AI 出于安全原因已禁用 `expression`，`function` 是 Python 函数；这里只返回 `default`。
    Computed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Text,
    Attribute(String),
    Html,
    Regex { pattern: String, group: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Lowercase,
    Uppercase,
    Strip,
}

impl Transform {
    pub(crate) fn apply(self, s: &str) -> String {
        match self {
            Transform::Lowercase => s.to_lowercase(),
            Transform::Uppercase => s.to_uppercase(),
            Transform::Strip => s.trim().to_owned(),
        }
    }
}

impl Schema {
    pub fn from_json(value: &Value) -> Result<Self, Error> {
        let obj = as_object(value, "$")?;
        let base_selector = match obj.get("baseSelector") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
            Some(Value::String(_)) => return Err(schema_err("baseSelector", "must not be empty")),
            Some(_) => return Err(schema_err("baseSelector", "must be a string")),
            None => return Err(schema_err("baseSelector", "is required")),
        };
        let base_fields = match obj.get("baseFields") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => parse_fields(v, "baseFields")?,
        };
        let fields = match obj.get("fields") {
            Some(v) => parse_fields(v, "fields")?,
            None if !base_fields.is_empty() => Vec::new(),
            None => return Err(schema_err("fields", "is required")),
        };
        Ok(Schema {
            base_selector,
            base_fields,
            fields,
        })
    }
}

impl FromStr for Schema {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let value: Value = serde_json::from_str(s).map_err(|e| Error::Json(e.to_string()))?;
        Schema::from_json(&value)
    }
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        Schema::from_json(&value).map_err(serde::de::Error::custom)
    }
}

fn schema_err(path: &str, message: impl Into<String>) -> Error {
    Error::Schema {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn as_object<'a>(v: &'a Value, path: &str) -> Result<&'a Map<String, Value>, Error> {
    v.as_object()
        .ok_or_else(|| schema_err(path, format!("expected an object, got {}", kind_of(v))))
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn parse_fields(v: &Value, path: &str) -> Result<Vec<Field>, Error> {
    let arr = v
        .as_array()
        .ok_or_else(|| schema_err(path, format!("expected an array, got {}", kind_of(v))))?;
    arr.iter()
        .enumerate()
        .map(|(i, f)| parse_field(f, &format!("{path}[{i}]")))
        .collect()
}

fn opt_string(obj: &Map<String, Value>, key: &str, path: &str) -> Result<Option<String>, Error> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(v) => Err(schema_err(
            &format!("{path}.{key}"),
            format!("expected a string, got {}", kind_of(v)),
        )),
    }
}

fn parse_field(v: &Value, path: &str) -> Result<Field, Error> {
    let obj = as_object(v, path)?;
    let name = opt_string(obj, "name", path)?
        .ok_or_else(|| schema_err(&format!("{path}.name"), "is required"))?;
    let selector = opt_string(obj, "selector", path)?;
    let source = opt_string(obj, "source", path)?;
    if let Some(src) = &source
        && !src.trim_start().starts_with('+')
    {
        return Err(schema_err(
            &format!("{path}.source"),
            "only the sibling form \"+ <selector>\" is supported",
        ));
    }
    let transform = match opt_string(obj, "transform", path)?.as_deref() {
        None => None,
        Some("lowercase") => Some(Transform::Lowercase),
        Some("uppercase") => Some(Transform::Uppercase),
        Some("strip") => Some(Transform::Strip),
        Some(other) => {
            return Err(schema_err(
                &format!("{path}.transform"),
                format!("unknown transform {other:?} (expected lowercase, uppercase or strip)"),
            ));
        }
    };
    let type_path = format!("{path}.type");
    let ty = obj
        .get("type")
        .ok_or_else(|| schema_err(&type_path, "is required"))?;
    let kind = match ty {
        Value::String(t) => match t.as_str() {
            "nested" | "list" | "nested_list" => {
                if selector.is_none() {
                    return Err(schema_err(
                        &format!("{path}.selector"),
                        format!("is required for type {t:?}"),
                    ));
                }
                let sub_path = format!("{path}.fields");
                let sub = parse_fields(
                    obj.get("fields").ok_or_else(|| {
                        schema_err(&sub_path, format!("is required for type {t:?}"))
                    })?,
                    &sub_path,
                )?;
                match t.as_str() {
                    "nested" => FieldKind::Nested(sub),
                    "list" => FieldKind::List(sub),
                    _ => FieldKind::NestedList(sub),
                }
            }
            "computed" => FieldKind::Computed,
            step => FieldKind::Value(vec![parse_step(step, obj, path, &type_path)?]),
        },
        Value::Array(steps) if !steps.is_empty() => {
            let mut out = Vec::with_capacity(steps.len());
            for (i, s) in steps.iter().enumerate() {
                let p = format!("{type_path}[{i}]");
                let s = s.as_str().ok_or_else(|| {
                    schema_err(&p, format!("expected a string, got {}", kind_of(s)))
                })?;
                out.push(parse_step(s, obj, path, &p)?);
            }
            FieldKind::Value(out)
        }
        Value::Array(_) => return Err(schema_err(&type_path, "type pipeline must not be empty")),
        v => {
            return Err(schema_err(
                &type_path,
                format!(
                    "expected a string or an array of strings, got {}",
                    kind_of(v)
                ),
            ));
        }
    };
    Ok(Field {
        name,
        selector,
        source,
        kind,
        default: obj.get("default").cloned(),
        transform,
    })
}

fn parse_step(
    step: &str,
    obj: &Map<String, Value>,
    path: &str,
    type_path: &str,
) -> Result<Step, Error> {
    Ok(match step {
        "text" => Step::Text,
        "html" => Step::Html,
        "attribute" => Step::Attribute(opt_string(obj, "attribute", path)?.ok_or_else(|| {
            schema_err(
                &format!("{path}.attribute"),
                "is required for type \"attribute\"",
            )
        })?),
        "regex" => {
            let pattern = opt_string(obj, "pattern", path)?.ok_or_else(|| {
                schema_err(&format!("{path}.pattern"), "is required for type \"regex\"")
            })?;
            let group = match obj.get("group") {
                None | Some(Value::Null) => 1,
                Some(v) => v
                    .as_u64()
                    .and_then(|g| usize::try_from(g).ok())
                    .ok_or_else(|| {
                        schema_err(&format!("{path}.group"), "expected a non-negative integer")
                    })?,
            };
            Step::Regex { pattern, group }
        }
        other => {
            return Err(schema_err(
                type_path,
                format!(
                    "unknown field type {other:?} (expected text, attribute, html, regex, nested, list, nested_list or computed)"
                ),
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> String {
        s.parse::<Schema>().unwrap_err().to_string()
    }

    #[test]
    fn parses_full_schema() {
        let s: Schema = r#"{
            "name": "x", "baseSelector": "div", "baseFields": [{"name": "id", "type": "attribute", "attribute": "data-id"}],
            "fields": [
                {"name": "a", "selector": "a", "type": ["attribute", "regex"], "attribute": "href", "pattern": "id=(\\d+)", "default": 0},
                {"name": "n", "selector": "p", "type": "nested", "fields": [{"name": "t", "type": "text", "transform": "uppercase"}]},
                {"name": "c", "type": "computed", "expression": "1+1"}
            ]}"#
        .parse()
        .unwrap();
        assert_eq!(s.base_fields.len(), 1);
        assert_eq!(
            s.fields[0].kind,
            FieldKind::Value(vec![
                Step::Attribute("href".into()),
                Step::Regex {
                    pattern: "id=(\\d+)".into(),
                    group: 1
                }
            ])
        );
        assert_eq!(s.fields[0].default, Some(Value::from(0)));
        assert!(matches!(s.fields[1].kind, FieldKind::Nested(_)));
        assert_eq!(s.fields[2].kind, FieldKind::Computed);
    }

    #[test]
    fn reports_clear_errors() {
        assert!(err("{").contains("not valid JSON"));
        assert!(err("[]").contains("expected an object"));
        assert!(err(r#"{"fields": []}"#).contains("`baseSelector`: is required"));
        assert!(err(r#"{"baseSelector": "a"}"#).contains("`fields`: is required"));
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"type": "text"}]}"#)
                .contains("`fields[0].name`")
        );
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"name": "x", "type": "txt"}]}"#)
                .contains("unknown field type \"txt\"")
        );
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"name": "x", "type": "attribute"}]}"#)
                .contains("`fields[0].attribute`: is required")
        );
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"name": "x", "type": "list", "selector": "li"}]}"#)
                .contains("`fields[0].fields`: is required")
        );
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"name": "x", "type": "nested", "fields": []}]}"#)
                .contains("`fields[0].selector`: is required")
        );
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"name": "x", "type": "text", "transform": "title"}]}"#)
                .contains("unknown transform")
        );
        assert!(
            err(r#"{"baseSelector": "a", "fields": [{"name": "x", "type": ["text", 3]}]}"#)
                .contains("`fields[0].type[1]`")
        );
    }

    #[test]
    fn deserializes_inside_other_structs() {
        #[derive(Deserialize)]
        struct Req {
            schema: Schema,
        }
        let r: Req =
            serde_json::from_str(r#"{"schema": {"baseSelector": "a", "fields": []}}"#).unwrap();
        assert_eq!(r.schema.base_selector, "a");
        let e = serde_json::from_str::<Req>(r#"{"schema": {"fields": []}}"#)
            .err()
            .unwrap();
        assert!(e.to_string().contains("baseSelector"));
    }
}
