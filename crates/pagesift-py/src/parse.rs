//! 解析调用方给的文件（`Scraper.parse`、`Scraper.parse_many`）：PDF、HTML、Word、Excel、
//! PowerPoint、EPUB、CSV，走和 HTTP `/v2/parse` 相同的 `pagesift::Scraper::parse`，结果和抓取一样是 `Document`。

use pagesift::Scraper;
use pagesift::api::types::{ScrapeOptions, Upload, UploadKind};
use pagesift::api::{ApiError, check_parse_options};

use crate::engine::Outcome;

/// 一个待解析的文件。字节在拿到 GIL 时复制出来，之后的识别和解析都不需要 GIL。
pub struct FileInput {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub content_type: Option<String>,
    /// 页面原地址：HTML 的相对链接按它解析，也作为 `metadata.source_url`；不会去抓。
    pub url: Option<String>,
}

/// 选项是否适用于上传文件（和 HTTP `/v2/parse` 同样的规则：不支持浏览器渲染、条件请求等）。
pub fn check_options(scraper: &Scraper, opts: &ScrapeOptions) -> Result<(), ApiError> {
    check_parse_options(opts, scraper)
}

/// 按内容、文件名和 Content-Type 识别类型，组成 `Upload`；认不出的类型不猜，直接拒绝。
fn upload(f: FileInput) -> Result<Upload, ApiError> {
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

/// 解析一个文件。`Outcome.url` 是文件名（批量结果里用它对应输入）。
pub async fn parse_one(
    scraper: &Scraper,
    index: Option<usize>,
    f: FileInput,
    opts: &ScrapeOptions,
) -> Outcome {
    let url = f.filename.clone();
    let result = match upload(f) {
        Ok(u) => scraper.parse(u, opts).await,
        Err(e) => Err(e),
    };
    Outcome { index, url, result }
}
