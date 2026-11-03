//! 解析调用方给的文件（`Scraper.parse`、`Scraper.parseStream`）：PDF、HTML、Word、Excel、
//! PowerPoint、EPUB、CSV，走和 HTTP `/v2/parse` 相同的 `pagesift::Scraper::parse`，结果和抓取一样是 `Page`。

use napi::bindgen_prelude::*;
use napi_derive::napi;
use pagesift::api::types::{ScrapeOptions, Upload, UploadKind};
use pagesift::api::{ApiError, check_parse_options};

use crate::error::Fallible;
use crate::page::Page;

/// `parseStream` 的一个输入文件。
#[napi(object, object_to_js = false)]
pub struct ParseFile {
    /// 文件内容（`Buffer` 也可以）。
    pub data: Uint8Array,
    /// 文件名：内容认不出类型时按扩展名判断，也作为结果的 `metadata.sourceURL`。
    pub filename: String,
    /// 文件的 Content-Type，按内容和扩展名都认不出时再看它。
    pub content_type: Option<String>,
    /// 页面原地址：HTML 的相对链接按它解析，也作为 `metadata.sourceURL`；不会去抓。
    pub url: Option<String>,
}

/// 一个待解析的文件。字节在主线程上复制出来，之后的识别和解析都在 Rust 线程上。
pub struct FileInput {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub content_type: Option<String>,
    pub url: Option<String>,
}

impl From<ParseFile> for FileInput {
    fn from(f: ParseFile) -> Self {
        Self {
            bytes: f.data.to_vec(),
            filename: f.filename,
            content_type: f.content_type,
            url: f.url,
        }
    }
}

/// 选项是否适用于上传文件（和 HTTP `/v2/parse` 同样的规则：不支持浏览器渲染、条件请求等）。
pub fn check_options(core: &pagesift::Scraper, opts: &ScrapeOptions) -> Fallible<()> {
    check_parse_options(opts, core)
}

/// 按内容、文件名和 Content-Type 识别类型，组成 `Upload`；认不出的类型不猜，直接拒绝。
fn upload(f: FileInput) -> Fallible<Upload> {
    let url = match f.url {
        Some(u) => Some(
            url::Url::parse(&u)
                .map_err(|e| ApiError::invalid(format!("invalid url {u:?}: {e}")))?,
        ),
        None => None,
    };
    let Some(kind) = UploadKind::detect(&f.bytes, &f.filename, f.content_type.as_deref()) else {
        return Err(ApiError::unsupported(format!(
            "unsupported file type: {}",
            f.filename
        )));
    };
    Ok(Upload {
        bytes: f.bytes,
        filename: f.filename,
        content_type: f.content_type,
        kind,
        url,
    })
}

pub async fn parse_one(
    core: &pagesift::Scraper,
    f: FileInput,
    opts: &ScrapeOptions,
) -> Fallible<Page> {
    core.parse(upload(f)?, opts).await.map(Page::from_scrape)
}
