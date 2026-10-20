//! JSON-RPC 2.0 消息：严格解析、响应和标准错误码。

use std::fmt;

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Map, Value};

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// 请求 ID。MCP 只允许字符串或整数，不允许 `null`。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Id {
    Int(i64),
    Str(String),
}

impl Id {
    /// 从 JSON 值解析 ID；不是字符串或 i64 范围内的整数就返回 `None`。
    pub fn from_value(v: &Value) -> Option<Id> {
        match v {
            Value::String(s) => Some(Id::Str(s.clone())),
            Value::Number(n) => n.as_i64().map(Id::Int),
            _ => None,
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Id::Int(n) => Value::from(*n),
            Id::Str(s) => Value::from(s.as_str()),
        }
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Int(n) => write!(f, "{n}"),
            Id::Str(s) => write!(f, "{s:?}"),
        }
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Id::Int(n) => s.serialize_i64(*n),
            Id::Str(v) => s.serialize_str(v),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Id,
    pub method: String,
    /// 对象或数组；省略时为 `None`。
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Option<Value>,
}

/// 对端发来的消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    /// 客户端发来的响应。本服务器不向客户端发请求，收到后忽略。
    Response(Response),
}

/// 一次读取的内容：单条消息或批量。批量里的每一项单独校验。
#[derive(Debug)]
pub enum Parsed {
    Single(Message),
    Batch(Vec<Result<Message, Response>>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl ErrorObject {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(INVALID_REQUEST, message)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, message)
    }
}

impl Serialize for ErrorObject {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("code", &self.code)?;
        m.serialize_entry("message", &self.message)?;
        if let Some(d) = &self.data {
            m.serialize_entry("data", d)?;
        }
        m.end()
    }
}

/// 响应。`id` 为 `None` 时序列化成 `"id": null`（请求 ID 读不出来的情况）。
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub id: Option<Id>,
    pub payload: Result<Value, ErrorObject>,
}

impl Response {
    pub fn result(id: Id, result: Value) -> Self {
        Self {
            id: Some(id),
            payload: Ok(result),
        }
    }

    pub fn error(id: Option<Id>, error: ErrorObject) -> Self {
        Self {
            id,
            payload: Err(error),
        }
    }

    pub fn is_error(&self) -> bool {
        self.payload.is_err()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

impl Serialize for Response {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("jsonrpc", "2.0")?;
        m.serialize_entry("id", &self.id)?;
        match &self.payload {
            Ok(r) => m.serialize_entry("result", r)?,
            Err(e) => m.serialize_entry("error", e)?,
        }
        m.end()
    }
}

/// 构造一条通知消息。
pub fn notification(method: &str, params: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// 解析一段文本：语法错误返回 `-32700`，结构错误返回 `-32600`。
pub fn parse(text: &[u8]) -> Result<Parsed, Response> {
    let v: Value = serde_json::from_slice(text).map_err(|e| {
        Response::error(
            None,
            ErrorObject::new(PARSE_ERROR, format!("Parse error: {e}")),
        )
    })?;
    parse_value(v)
}

/// 和 [`parse`] 相同，输入是已经解析好的 JSON。
pub fn parse_value(v: Value) -> Result<Parsed, Response> {
    match v {
        Value::Array(items) => {
            if items.is_empty() {
                return Err(Response::error(
                    None,
                    ErrorObject::invalid_request("Invalid Request: empty batch"),
                ));
            }
            Ok(Parsed::Batch(
                items.into_iter().map(parse_message).collect(),
            ))
        }
        other => parse_message(other).map(Parsed::Single),
    }
}

/// 校验单条消息。
pub fn parse_message(v: Value) -> Result<Message, Response> {
    let Value::Object(mut obj) = v else {
        return Err(invalid(None, "message must be a JSON object"));
    };
    // 能读出 ID 就在错误响应里带上。
    let id_value = obj.remove("id");
    let id = id_value.as_ref().and_then(Id::from_value);
    match obj.remove("jsonrpc") {
        Some(Value::String(s)) if s == "2.0" => {}
        _ => return Err(invalid(id, "\"jsonrpc\" must be exactly \"2.0\"")),
    }

    if let Some(method) = obj.remove("method") {
        let Value::String(method) = method else {
            return Err(invalid(id, "\"method\" must be a string"));
        };
        let params = match obj.remove("params") {
            None => None,
            Some(p @ (Value::Object(_) | Value::Array(_))) => Some(p),
            Some(_) => return Err(invalid(id, "\"params\" must be an object or array")),
        };
        if let Some(key) = obj.keys().next() {
            return Err(invalid(id, &format!("unexpected member \"{key}\"")));
        }
        return match id_value {
            None => Ok(Message::Notification(Notification { method, params })),
            Some(raw) => match id {
                Some(id) => Ok(Message::Request(Request { id, method, params })),
                None => Err(invalid(
                    None,
                    &format!("\"id\" must be a string or an integer, got {raw}"),
                )),
            },
        };
    }

    // 没有 method：只能是响应。
    let result = obj.remove("result");
    let error = obj.remove("error");
    if let Some(key) = obj.keys().next() {
        return Err(invalid(id, &format!("unexpected member \"{key}\"")));
    }
    let payload = match (result, error) {
        (Some(r), None) => Ok(r),
        (None, Some(e)) => Err(parse_error_object(e)
            .ok_or_else(|| invalid(id.clone(), "malformed \"error\" member"))?),
        (None, None) => return Err(invalid(id, "missing \"method\"")),
        (Some(_), Some(_)) => return Err(invalid(id, "both \"result\" and \"error\" present")),
    };
    match (id_value, id) {
        (Some(_), Some(id)) => Ok(Message::Response(Response {
            id: Some(id),
            payload,
        })),
        (Some(Value::Null), None) if payload.is_err() => {
            Ok(Message::Response(Response { id: None, payload }))
        }
        _ => Err(invalid(
            None,
            "response \"id\" must be a string or an integer",
        )),
    }
}

fn parse_error_object(v: Value) -> Option<ErrorObject> {
    let Value::Object(mut o) = v else { return None };
    let code = o
        .remove("code")?
        .as_i64()
        .and_then(|c| i32::try_from(c).ok())?;
    let Value::String(message) = o.remove("message")? else {
        return None;
    };
    Some(ErrorObject {
        code,
        message,
        data: o.remove("data"),
    })
}

fn invalid(id: Option<Id>, why: &str) -> Response {
    Response::error(
        id,
        ErrorObject::invalid_request(format!("Invalid Request: {why}")),
    )
}

/// 取出对象形式的 params；省略时视为空对象，数组形式返回 `-32602`。
pub fn params_object(params: Option<&Value>) -> Result<Map<String, Value>, ErrorObject> {
    match params {
        None => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        Some(_) => Err(ErrorObject::invalid_params("params must be an object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn single(text: &str) -> Result<Message, Response> {
        match parse(text.as_bytes()) {
            Ok(Parsed::Single(m)) => Ok(m),
            Ok(Parsed::Batch(_)) => panic!("unexpected batch"),
            Err(e) => Err(e),
        }
    }

    fn code(r: Result<Message, Response>) -> i32 {
        r.unwrap_err().payload.unwrap_err().code
    }

    #[test]
    fn parses_request_notification_response() {
        let m = single(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).unwrap();
        assert_eq!(
            m,
            Message::Request(Request {
                id: Id::Int(7),
                method: "ping".into(),
                params: None
            })
        );
        let m = single(r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#)
            .unwrap();
        assert!(matches!(m, Message::Notification(n) if n.method == "notifications/initialized"));
        let m = single(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#).unwrap();
        assert!(matches!(m, Message::Response(r) if r.id == Some(Id::Str("a".into()))));
        let m = single(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-1,"message":"x"}}"#).unwrap();
        assert!(matches!(m, Message::Response(r) if r.id.is_none() && r.is_error()));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(code(single("{")), PARSE_ERROR);
        assert_eq!(code(single("[1")), PARSE_ERROR);
        assert_eq!(code(single("42")), INVALID_REQUEST);
        assert_eq!(code(single(r#"{"id":1,"method":"x"}"#)), INVALID_REQUEST);
        assert_eq!(
            code(single(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#)),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(r#"{"jsonrpc":2.0,"id":1,"method":"x"}"#)),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(r#"{"jsonrpc":"2.0","id":1,"method":5}"#)),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#)),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(r#"{"jsonrpc":"2.0","id":1.5,"method":"x"}"#)),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(r#"{"jsonrpc":"2.0","id":true,"method":"x"}"#)),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(
                r#"{"jsonrpc":"2.0","id":1,"method":"x","params":3}"#
            )),
            INVALID_REQUEST
        );
        assert_eq!(
            code(single(r#"{"jsonrpc":"2.0","id":1,"method":"x","extra":1}"#)),
            INVALID_REQUEST
        );
        assert_eq!(code(single(r#"{"jsonrpc":"2.0","id":1}"#)), INVALID_REQUEST);
        assert_eq!(
            code(single(r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{}}"#)),
            INVALID_REQUEST
        );
        assert_eq!(code(parse(b"[]").map(|_| unreachable!())), INVALID_REQUEST);
    }

    #[test]
    fn error_keeps_readable_id() {
        let e = single(r#"{"jsonrpc":"1.0","id":"x","method":"m"}"#).unwrap_err();
        assert_eq!(e.id, Some(Id::Str("x".into())));
        let e = single(r#"{"jsonrpc":"2.0","id":[1],"method":"m"}"#).unwrap_err();
        assert_eq!(e.to_value()["id"], Value::Null);
    }

    #[test]
    fn batch_validates_each_item() {
        let Ok(Parsed::Batch(items)) =
            parse(br#"[{"jsonrpc":"2.0","id":1,"method":"a"},1,{"jsonrpc":"2.0","method":"b"}]"#)
        else {
            panic!("expected batch");
        };
        assert!(matches!(items[0], Ok(Message::Request(_))));
        assert!(items[1].is_err());
        assert!(matches!(items[2], Ok(Message::Notification(_))));
    }

    #[test]
    fn response_serialization() {
        let r = Response::result(Id::Int(1), json!({"ok": true}));
        assert_eq!(
            r.to_value(),
            json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}})
        );
        let r = Response::error(None, ErrorObject::new(-32700, "bad").with_data(json!(1)));
        assert_eq!(
            r.to_value(),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad","data":1}})
        );
    }

    #[test]
    fn params_object_rules() {
        assert!(params_object(None).unwrap().is_empty());
        assert_eq!(
            params_object(Some(&json!([1]))).unwrap_err().code,
            INVALID_PARAMS
        );
    }
}
