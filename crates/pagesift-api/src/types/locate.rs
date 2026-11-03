//! `/v2/observe`、`/v2/similar` 的请求和响应（pagesift 扩展）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// observe 的结果：候选元素、用量警告。
pub type Observed = (Vec<Value>, Vec<String>);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserveRequest {
    pub url: String,
    pub instruction: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObserveResponse {
    pub success: bool,
    /// 候选元素，最合适的在前：`{selector, role, name}`。
    pub data: Vec<Value>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarRequest {
    pub url: String,
    /// 例子元素的 CSS 选择器（取第一个命中的）。
    pub selector: String,
    #[serde(default)]
    pub engine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SimilarItem {
    pub text: String,
    pub html: String,
}

/// 例子没有结构相同的兄弟时 `items` 为空，`selector`、`depth` 为 null。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SimilarData {
    pub selector: Option<String>,
    pub depth: Option<usize>,
    pub items: Vec<SimilarItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SimilarResponse {
    pub success: bool,
    pub data: SimilarData,
    pub warnings: Vec<String>,
}
