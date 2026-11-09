//! `exportImages`：页面里的图片作为附件。
//!
//! - PDF 内嵌图片、Office 媒体在各自的转换里导出（`pdf.rs`、`office.rs`）。
//! - 网页图片在这里取回：走同一条礼貌抓取路径（站点限速、冷却、robots 按请求的模式、SSRF 由抓取器检查），
//!   一张一张地取；所有图片加起来不超过一页正文的上限（`pagesift_net::resources::body_cap`：用户上限与
//!   内存预算 / 期望在途页数取小），每张请求的上限是剩下的额度，超出的不导出。按内容哈希去重。
//! - 只在请求打开 `exportImages` 时下载，默认行为不变；导出时不读也不写结果缓存（缓存里的文档不带附件）。

use std::collections::HashSet;

use pagesift_api::types::Attachment;

use super::*;

impl Scraper {
    /// 取回 `doc.images` 里的网页图片，加进 `doc.attachments`。
    pub(super) async fn export_web_images(&self, doc: &mut Document, mode: RobotsMode) {
        let Some(urls) = doc.images.clone() else {
            return;
        };
        let user_cap = self.policy.fetcher().config().max_body_bytes;
        let mut budget = pagesift_net::resources::body_cap(user_cap);
        let mut seen_urls = HashSet::new();
        let mut seen: HashSet<String> = doc.attachments.iter().map(|a| a.hash.clone()).collect();
        let (mut tried, mut skipped) = (0usize, 0usize);
        for url in urls {
            if !(url.starts_with("http://") || url.starts_with("https://"))
                || !seen_urls.insert(url.clone())
            {
                continue;
            }
            tried += 1;
            if budget == 0 {
                skipped += 1;
                continue;
            }
            let mut req = FetchRequest::new(url.as_str());
            req.max_body_bytes = Some(budget);
            let resp = match self.policy.fetch(req, mode).await {
                // [协议] 2xx 成功（RFC 9110 §15.3）
                Ok(r) if (200..300).contains(&r.status) && !r.truncated => r,
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            let Some(format) = image_format(&resp.body, resp.content_type()) else {
                skipped += 1;
                continue;
            };
            budget = budget.saturating_sub(resp.body.len());
            let a = Attachment::new(resp.body, format, Some(url));
            if seen.insert(a.hash.clone()) {
                doc.attachments.push(a);
            }
        }
        if skipped > 0
            && let Some(meta) = doc.metadata.pagesift.as_mut()
        {
            meta.warnings.push(format!(
                "images_not_exported: {skipped} of {tried} images (robots, size limit, fetch error or not an image)"
            ));
        }
    }
}

/// 按文件签名判断图片格式；不信扩展名或来源提供的 Content-Type。
pub(super) fn image_format(body: &[u8], _content_type: Option<&str>) -> Option<&'static str> {
    pagesift_api::types::attachment::sniff_image_format(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_come_from_content_not_headers() {
        assert_eq!(image_format(b"\x89PNG\r\n\x1a\n....", None), Some("png"));
        assert_eq!(
            image_format(&[0xFF, 0xD8, 0xFF, 0xE0], Some("text/html")),
            Some("jpg")
        );
        assert_eq!(image_format(b"RIFF\0\0\0\0WEBPVP8 ", None), Some("webp"));
        assert_eq!(
            image_format(b"<svg xmlns=...>", Some("image/svg+xml; charset=utf-8")),
            Some("svg")
        );
        assert_eq!(image_format(b"<html>", Some("text/html")), None);
        assert_eq!(image_format(b"<html>", Some("image/svg+xml")), None);
        assert_eq!(image_format(b"not an image", Some("image/tiff")), None);
        assert_eq!(
            image_format(
                b"\xef\xbb\xbf <?xml version=\"1.0\"?>\n<!--x--><svg viewBox=\"0 0 1 1\"/>",
                None
            ),
            Some("svg")
        );
        assert_eq!(image_format(b"<svgfoo/>", Some("image/svg+xml")), None);
        assert_eq!(image_format(b"II\x2a\0payload", None), Some("tif"));
        assert_eq!(image_format(b"\0\0\0\x0cjP  \r\n\x87\n", None), Some("jp2"));
    }
}
