//! 资源：工具结果里的 `resource_link` 指向的内容，客户端用 `resources/read` 取
//! （MCP 2025-06-18 规范 Server Features → Resources、Tools → Resource Links）。
//!
//! 只读、按 URI 取；`resources/list` 与 `resources/templates/list` 返回空列表，不支持订阅。

use base64::Engine as _;
use serde_json::{Value, json};

use crate::jsonrpc::ErrorObject;

/// 按 URI 读资源的提供者。
pub trait ResourceReader: Send + Sync + 'static {
    /// 读一个资源：（MIME 类型，内容）。不存在或已过期时为 `None`。
    fn read(&self, uri: &str) -> Option<(String, Vec<u8>)>;
}

/// [协议] MCP 规范 Resources → Error Handling：资源不存在用 -32002
const RESOURCE_NOT_FOUND: i32 = -32002;

/// `resources/read` 的结果：二进制内容用 base64 的 `blob`。
pub(crate) fn read(reader: &dyn ResourceReader, params: &Value) -> Result<Value, ErrorObject> {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return Err(ErrorObject::invalid_params(
            "resources/read: \"uri\" is required",
        ));
    };
    match reader.read(uri) {
        Some((mime, data)) => Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": mime,
                "blob": base64::engine::general_purpose::STANDARD.encode(data),
            }]
        })),
        None => Err(
            ErrorObject::new(RESOURCE_NOT_FOUND, format!("resource not found: {uri}"))
                .with_data(json!({ "uri": uri })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct One;
    impl ResourceReader for One {
        fn read(&self, uri: &str) -> Option<(String, Vec<u8>)> {
            (uri == "pagesift://attachments/a.png").then(|| ("image/png".into(), vec![1, 2, 3]))
        }
    }

    #[test]
    fn reads_blobs_and_reports_missing_resources() {
        let v = read(&One, &json!({ "uri": "pagesift://attachments/a.png" })).unwrap();
        assert_eq!(v["contents"][0]["blob"], "AQID");
        assert_eq!(v["contents"][0]["mimeType"], "image/png");
        let e = read(&One, &json!({ "uri": "pagesift://attachments/b.png" })).unwrap_err();
        assert_eq!(e.code, RESOURCE_NOT_FOUND);
        assert!(read(&One, &json!({})).is_err());
    }
}
