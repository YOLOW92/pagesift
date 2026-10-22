//! 网页形态类型定义与置信度评估。

use serde::{Deserialize, Serialize};

/// 页面内容形态分类（对标 WCXB 7 大标准品类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageType {
    Article,
    Forum,
    Product,
    Documentation,
    Listing,
    Collection,
    Service,
    General,
}

impl PageType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Article => "article",
            Self::Forum => "forum",
            Self::Product => "product",
            Self::Documentation => "documentation",
            Self::Listing => "listing",
            Self::Collection => "collection",
            Self::Service => "service",
            Self::General => "general",
        }
    }
}

impl std::fmt::Display for PageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
