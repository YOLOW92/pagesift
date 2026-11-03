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

/// 按文件签名判断图片格式（各格式规范的魔数），认不出时看 Content-Type；都不是图片返回 `None`。
pub(super) fn image_format(body: &[u8], content_type: Option<&str>) -> Option<&'static str> {
    let starts = |sig: &[u8]| body.starts_with(sig);
    // [协议] PNG（RFC 2083 §3.1）、JPEG（ITU-T T.81 SOI）、GIF87a/89a、RIFF…WEBP、BMP、ICO 的文件签名
    let sniffed = if starts(b"\x89PNG\r\n\x1a\n") {
        Some("png")
        // [协议] JPEG：SOI 标记 FFD8 后接一个段标记 FF
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("gif")
        // [协议] RIFF 头 12 字节，第 8–11 字节是格式标识
    } else if starts(b"RIFF") && body.get(8..12) == Some(b"WEBP") {
        Some("webp")
        // [协议] ISO BMFF：第 4–7 字节 `ftyp`，接着是主品牌 `avif`
    } else if body.get(4..12) == Some(b"ftypavif") {
        Some("avif")
    } else if starts(b"BM") {
        Some("bmp")
    } else if starts(&[0, 0, 1, 0]) {
        Some("ico")
    } else {
        None
    };
    if sniffed.is_some() {
        return sniffed;
    }
    let ct = content_type?.split(';').next()?.trim().to_ascii_lowercase();
    match ct.as_str() {
        "image/svg+xml" => Some("svg"),
        "image/tiff" => Some("tif"),
        "image/avif" => Some("avif"),
        "image/jp2" => Some("jp2"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_come_from_signatures_then_content_type() {
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
    }
}
