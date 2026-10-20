/// 本 crate 的所有错误。`path` 指出 schema 里出错的位置，例如 `fields[1].fields[0].selector`。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// schema 不是合法 JSON。
    #[error("schema is not valid JSON: {0}")]
    Json(String),
    /// schema 结构错误：缺字段、类型不对、取值不认识。
    #[error("invalid schema at `{path}`: {message}")]
    Schema { path: String, message: String },
    /// CSS 或 XPath 选择器写错了。
    #[error("invalid selector at `{path}` ({selector:?}): {message}")]
    Selector {
        path: String,
        selector: String,
        message: String,
    },
    /// 正则写错了，或者用了 Rust `regex` 不支持的语法（环视、反向引用）。
    #[error("invalid regex at `{path}`: {message}")]
    Regex { path: String, message: String },
    /// XPath 求值失败（未知函数、参数个数不对等）。
    #[error("XPath evaluation failed at `{path}`: {message}")]
    XPath { path: String, message: String },
    /// 文档嵌套太深，XPath 求值会有栈溢出风险。
    #[error("document nesting depth {depth} exceeds the XPath limit of {limit}")]
    TooDeep { depth: usize, limit: usize },
    /// 分块、BM25 等参数不合法。
    #[error("invalid option: {0}")]
    Options(String),
}
