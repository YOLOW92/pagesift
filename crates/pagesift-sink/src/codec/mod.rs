//! 自描述的紧凑二进制格式，给 serde 用：缓存、任务结果这类只在 pagesift 内部读写的数据。
//!
//! 比 JSON 省的是字符串：长度前缀 + 原样字节，读的时候不用逐字节找转义、也不用反转义。
//! 和 JSON 一样自描述（每个值带类型标记），所以 `#[serde(flatten)]`、`skip_serializing_if`、
//! `serde_json::Value` 都能原样往返。map 和 seq 不写长度、以 `END` 结尾，flatten 时长度未知也能流式写出。
//!
//! 整数用 LEB128 变长编码（DWARF 5 §7.6），有符号数先做 zigzag 映射（Protocol Buffers 编码规范）。

mod de;
mod ser;

pub use de::from_slice;
pub use ser::{to_vec, to_writer};

/// 值的类型标记。[协议] 本格式自己的定义，改动要升缓存格式版本。
pub(crate) mod tag {
    pub const NULL: u8 = 0; // [协议]
    pub const FALSE: u8 = 1; // [协议]
    pub const TRUE: u8 = 2; // [协议]
    pub const UINT: u8 = 3; // [协议]
    pub const INT: u8 = 4; // [协议]
    pub const FLOAT: u8 = 5; // [协议]
    pub const STR: u8 = 6; // [协议]
    pub const BYTES: u8 = 7; // [协议]
    pub const SEQ: u8 = 8; // [协议]
    pub const MAP: u8 = 9; // [协议]
    pub const END: u8 = 10; // [协议]
}

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl serde::ser::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

impl serde::de::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    /// 附件在内部编码里连内容一起往返（多服务器的结果、缓存），JSON 里只有元数据。
    #[test]
    fn attachments_keep_their_bytes_inside_pagesift() {
        let mut doc = pagesift_api::types::Document::default();
        doc.attachments.push(pagesift_api::types::Attachment::new(
            vec![0xff, 0xd8, 0xff, 1, 2, 3],
            "jpg",
            Some("https://e.x/a.jpg".into()),
        ));
        let back: pagesift_api::types::Document = from_slice(&to_vec(&doc).unwrap()).unwrap();
        assert_eq!(back.attachments, doc.attachments);
        assert_eq!(
            back.attachments[0].data.as_deref(),
            Some(&[0xff, 0xd8, 0xff, 1, 2, 3][..])
        );
        let json = serde_json::to_value(&doc).unwrap();
        assert!(json["attachments"][0].get("data").is_none());
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Inner {
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        code: u16,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Kind {
        Unit,
        New(i32),
        Pair(u8, String),
        Named { a: f64 },
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Outer {
        inner: Inner,
        kinds: Vec<Kind>,
        maybe: Option<Option<bool>>,
        bytes: Vec<u8>,
        tuple: (char, i64),
        pairs: Vec<(String, String)>,
    }

    #[test]
    fn round_trips_flatten_options_enums_and_json_values() {
        let v = Outer {
            inner: Inner {
                title: None,
                code: 404,
                extra: BTreeMap::from([
                    ("ogImage".into(), serde_json::json!("x.png")),
                    (
                        "nested".into(),
                        serde_json::json!({"a": [1, -2, 3.5, null, true], "s": "引号\"\n"}),
                    ),
                ]),
            },
            kinds: vec![
                Kind::Unit,
                Kind::New(-7),
                Kind::Pair(3, "p".into()),
                Kind::Named { a: 0.25 },
            ],
            maybe: Some(Some(false)),
            bytes: vec![0, 255, 10],
            tuple: ('字', i64::MIN),
            pairs: vec![("en".into(), "https://e/en".into())],
        };
        let bytes = to_vec(&v).unwrap();
        assert_eq!(from_slice::<Outer>(&bytes).unwrap(), v);
        // 未知字段和截断都要报错，不能 panic
        assert!(from_slice::<Outer>(&bytes[..bytes.len() - 1]).is_err());
        let json = serde_json::to_value(&v.inner).unwrap();
        let back: serde_json::Value = from_slice(&to_vec(&json).unwrap()).unwrap();
        assert_eq!(back, json);
    }
}
