//! `Page`：一页的结果。内容留在 Rust 侧，读某个属性时才复制到 JS，没读的字段不产生拷贝。

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::Serialize;
use serde_json::Value;

/// 一页的转换或抓取结果。每次读属性都会新建一个 JS 值，需要多次使用时先存到变量里。
#[napi]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(skip_serializing_if = "Option::is_none")]
    markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    /// 导出的文件；`toJSON` 里只列元数据（字节不进 JSON）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<pagesift::api::types::Attachment>,
    metadata: Value,
}

/// 一个导出的文件（`exportImages`）。
#[napi(object)]
pub struct PageAttachment {
    /// 内容寻址的文件名：SHA-256 前 32 位十六进制加扩展名。
    pub id: String,
    /// 格式（扩展名）。
    pub format: String,
    /// 字节数。
    pub bytes: i64,
    /// 内容的 SHA-256（小写十六进制）。
    pub hash: String,
    /// 原地址或文档里的部件路径；PDF 图片为 null。
    pub source: Option<String>,
    /// 内容。
    pub data: Buffer,
}

impl Page {
    pub fn from_scrape(doc: pagesift::api::types::Document) -> Self {
        Self {
            markdown: doc.markdown,
            html: doc.html,
            raw_html: doc.raw_html,
            links: doc.links,
            images: doc.images,
            attachments: doc.attachments,
            metadata: serde_json::to_value(&doc.metadata).unwrap_or(Value::Null),
        }
    }

    pub fn new(
        markdown: Option<String>,
        html: Option<String>,
        raw_html: Option<String>,
        links: Option<Vec<String>>,
        images: Option<Vec<String>>,
        metadata: Value,
    ) -> Self {
        Self {
            markdown,
            html,
            raw_html,
            links,
            images,
            attachments: Vec::new(),
            metadata,
        }
    }
}

#[napi]
impl Page {
    /// 正文的 Markdown；`formats` 里没有 `markdown`、或条件请求命中（`notModified`）时为 null。
    #[napi(getter)]
    pub fn markdown(&self) -> Option<&str> {
        self.markdown.as_deref()
    }

    /// 清洗后的 HTML（`formats` 含 `html` 时才有）。
    #[napi(getter)]
    pub fn html(&self) -> Option<&str> {
        self.html.as_deref()
    }

    /// 原始 HTML（`formats` 含 `rawHtml` 时才有）。
    #[napi(getter)]
    pub fn raw_html(&self) -> Option<&str> {
        self.raw_html.as_deref()
    }

    /// 页面里的链接，绝对地址（`formats` 含 `links` 时才有）。
    #[napi(getter)]
    pub fn links(&self) -> Option<Vec<&str>> {
        self.links
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect())
    }

    /// 导出的文件（`exportImages: true`），每项带字节（`data`）。
    #[napi(getter)]
    pub fn attachments(&self) -> Vec<PageAttachment> {
        self.attachments
            .iter()
            .map(|a| PageAttachment {
                id: a.id.clone(),
                format: a.format.clone(),
                bytes: a.bytes as i64,
                hash: a.hash.clone(),
                source: a.source.clone(),
                data: a.data.as_deref().unwrap_or_default().to_vec().into(),
            })
            .collect()
    }

    /// 页面里的图片地址（`formats` 含 `images` 时才有）。
    #[napi(getter)]
    pub fn images(&self) -> Option<Vec<&str>> {
        self.images
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect())
    }

    /// 标题、描述、状态码、`og:*` 等元数据，以及 `pagesift` 扩展字段。
    #[napi(getter, ts_return_type = "PageMetadata")]
    pub fn metadata<'e>(&self, env: &'e Env) -> Result<Unknown<'e>> {
        env.to_js_value(&self.metadata)
    }

    /// 一次性取出全部字段，得到普通对象（`JSON.stringify(page)` 也会调用它）。
    #[napi(js_name = "toJSON", ts_return_type = "PageJSON")]
    pub fn to_json<'e>(&self, env: &'e Env) -> Result<Unknown<'e>> {
        env.to_js_value(self)
    }
}
