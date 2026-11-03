//! 请求体里的字段名：哪些是某个类型认得的，哪些没人认。
//!
//! serde 派生的 `Deserialize` 会把结构体的全部字段名（重命名之后的）交给
//! `Deserializer::deserialize_struct`。这里写一个只做这一件事的 Deserializer：收下字段表就报错返回，
//! 不真的解析任何东西。这样字段表和结构体定义永远一致，不用手写第二份。
//!
//! 用法有两种：
//! - pagesift 自己的入口（MCP、Python、Node 的 JSON 形参）用 [`reject_unknown`]：有未知字段就拒收，
//!   错误里列出字段名；
//! - 兼容 Firecrawl 的 `/v1`、`/v2` 用 [`unknown`]：照常接收，把没人认的字段名写进
//!   `warnings`（代码 `ignored_fields`）。

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, Visitor};
use serde_json::{Map, Value};

use crate::error::ApiError;

/// 警告代码：请求里有字段被忽略（兼容接口照常接收，但如实列出）。
pub const IGNORED_FIELDS: &str = "ignored_fields";

/// 客户端自报身份的字段（Firecrawl SDK 每次都带）：不是选项，不影响结果，也不算“被忽略”。
pub const CLIENT_IDENTITY: &[&str] = &["origin", "integration"];

#[derive(Debug)]
enum Probe {
    Fields(&'static [&'static str]),
    Other,
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("field probe")
    }
}

impl std::error::Error for Probe {}

impl de::Error for Probe {
    fn custom<T: fmt::Display>(_: T) -> Self {
        Probe::Other
    }
}

struct FieldProbe;

impl<'de> Deserializer<'de> for FieldProbe {
    type Error = Probe;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Probe> {
        Err(Probe::Other)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Probe> {
        Err(Probe::Fields(fields))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map enum identifier ignored_any
    }
}

/// `T` 认得的顶层字段名（serde 重命名之后的）。`T` 必须是不带 `flatten` 的派生结构体，
/// 否则返回空表（带 `flatten` 的结构体走的是 map，没有字段表）。
pub fn names<'de, T: Deserialize<'de>>() -> &'static [&'static str] {
    match T::deserialize(FieldProbe) {
        Err(Probe::Fields(f)) => f,
        _ => &[],
    }
}

/// 对象里不在 `known` 中的键（按 `serde_json::Map` 的键序，即字典序）。
pub fn unknown(obj: &Map<String, Value>, known: &[&[&str]]) -> Vec<String> {
    obj.keys()
        .filter(|k| !known.iter().any(|set| set.contains(&k.as_str())))
        .cloned()
        .collect()
}

/// 有未知字段就拒收，错误里列出全部未知字段名。
pub fn reject_unknown(obj: &Map<String, Value>, known: &[&[&str]]) -> Result<(), ApiError> {
    let bad = unknown(obj, known);
    if bad.is_empty() {
        return Ok(());
    }
    Err(ApiError::invalid(format!(
        "unknown field{} {}",
        if bad.len() == 1 { "" } else { "s" },
        bad.iter()
            .map(|b| format!("`{b}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .with_details(serde_json::json!({ "unknownFields": bad })))
}

/// `ignored_fields: a, b` 形式的警告；没有被忽略的字段时返回 `None`。
pub fn ignored_warning(ignored: &[String]) -> Option<String> {
    (!ignored.is_empty()).then(|| format!("{IGNORED_FIELDS}: {}", ignored.join(", ")))
}

/// 单页抓取请求（`url` 加 `ScrapeOptions` 的全部字段）认得的字段。
pub fn scrape_request() -> [&'static [&'static str]; 3] {
    [
        &["url"],
        names::<crate::types::ScrapeOptions>(),
        CLIENT_IDENTITY,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CrawlRequest, MapRequest, ScrapeOptions};
    use serde_json::json;

    #[test]
    fn captures_renamed_field_names() {
        let f = names::<ScrapeOptions>();
        assert!(f.contains(&"onlyMainContent") && f.contains(&"ifNoneMatch"));
        assert!(!f.contains(&"only_main_content"));
        assert!(names::<CrawlRequest>().contains(&"scrapeOptions"));
        assert!(names::<MapRequest>().contains(&"includeSubdomains"));
    }

    #[test]
    fn batch_own_fields_match_the_struct() {
        use crate::types::BatchScrapeRequest;
        let b: BatchScrapeRequest = serde_json::from_value(json!({"urls": ["u"]})).unwrap();
        let v = serde_json::to_value(&b).unwrap();
        let mut own: Vec<&str> = v
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|k| !names::<ScrapeOptions>().contains(k))
            .collect();
        own.sort_unstable();
        let mut want = BatchScrapeRequest::OWN_FIELDS.to_vec();
        want.sort_unstable();
        assert_eq!(own, want);
    }

    #[test]
    fn unknown_lists_every_stray_key() {
        let body = json!({"url": "u", "formats": [], "zeroDataRetention": true, "origin": "sdk", "lockdown": 1});
        let obj = body.as_object().unwrap();
        assert_eq!(
            unknown(obj, &scrape_request()),
            ["lockdown", "zeroDataRetention"]
        );
        let err = reject_unknown(obj, &scrape_request()).unwrap_err();
        assert!(err.message.contains("`lockdown`, `zeroDataRetention`"));
        assert_eq!(
            ignored_warning(&["a".into(), "b".into()]).unwrap(),
            "ignored_fields: a, b"
        );
    }
}
