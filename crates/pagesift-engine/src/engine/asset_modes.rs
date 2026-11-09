//! Image asset presentation after content extraction.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures_util::{StreamExt, stream};

use pagesift_api::types::{
    Attachment, Document, ImageAssetMode, ImageAssetResult, ImageAssetStatus,
};
use pagesift_net::FetchRequest;
use pagesift_policy::{ErrorCode, RobotsMode};

use super::Scraper;

// Asset retrieval must not hold a completed extraction hostage indefinitely.
const MAX_IMAGE_REQUESTS: usize = 64;
const IMAGE_CONCURRENCY: usize = 4;
const IMAGE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

fn result(source: String, status: ImageAssetStatus, reason: Option<String>) -> ImageAssetResult {
    ImageAssetResult {
        source,
        status,
        id: None,
        mime: None,
        bytes: None,
        reason,
    }
}

fn success(source: String, a: &Attachment) -> ImageAssetResult {
    ImageAssetResult {
        source,
        status: ImageAssetStatus::Ok,
        id: Some(a.id.clone()),
        mime: Some(a.mime().into()),
        bytes: Some(a.bytes),
        reason: None,
    }
}

/// Visit image destinations in Markdown emitted by the content/PDF/Office
/// renderers. Fenced and inline code are ignored; generated destinations have
/// parentheses and spaces percent-encoded and may be followed by a title.
fn image_destinations(markdown: &str, mut visit: impl FnMut(std::ops::Range<usize>)) {
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;
    for line in markdown.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let trimmed = line.trim_start_matches(' ');
        let marker = trimmed.as_bytes().first().copied();
        if matches!(marker, Some(b'`' | b'~')) {
            let marker = marker.unwrap();
            let count = trimmed
                .as_bytes()
                .iter()
                .take_while(|&&b| b == marker)
                .count();
            if count >= 3 {
                match fence {
                    None => fence = Some((marker, count)),
                    Some((kind, n)) if kind == marker && count >= n => fence = None,
                    _ => {}
                }
                offset += line.len();
                continue;
            }
        }
        if fence.is_some() {
            offset += line.len();
            continue;
        }
        let mut i = 0;
        let mut ticks = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i = (i + 2).min(bytes.len());
                continue;
            }
            if bytes[i] == b'`' {
                let count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                if ticks == 0 {
                    ticks = count;
                } else if ticks == count {
                    ticks = 0;
                }
                i += count;
                continue;
            }
            if ticks == 0 && bytes[i..].starts_with(b"![") {
                if let Some(end) = bytes[i + 2..].windows(2).position(|w| w == b"](") {
                    let start = i + 2 + end + 2;
                    let mut finish = start;
                    while finish < bytes.len()
                        && !matches!(bytes[finish], b')' | b' ' | b'\t' | b'\r' | b'\n')
                    {
                        finish += 1;
                    }
                    if finish > start && finish < bytes.len() {
                        visit(offset + start..offset + finish);
                        i = finish;
                        continue;
                    }
                    if finish == bytes.len() {
                        break;
                    }
                    i = start + 1;
                    continue;
                } else {
                    // No later image can have a closing delimiter on this line.
                    break;
                }
            }
            i += 1;
        }
        offset += line.len();
    }
}

fn rewrite(markdown: &str, replacements: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut start = 0;
    image_destinations(markdown, |span| {
        if let Some(replacement) = replacements.get(&markdown[span.clone()]) {
            out.push_str(&markdown[start..span.start]);
            out.push_str(replacement);
            start = span.end;
        }
    });
    out.push_str(&markdown[start..]);
    out
}

fn markdown_url(url: &str) -> String {
    url.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

fn add_asset(
    doc: &mut Document,
    mode: ImageAssetMode,
    source: String,
    dest: String,
    mut attachment: Attachment,
    replacements: &mut HashMap<String, String>,
    seen: &mut HashSet<String>,
) {
    if mode == ImageAssetMode::Embed && attachment.format == "svg" {
        doc.image_assets.push(result(
            source,
            ImageAssetStatus::Failed,
            Some("svg_not_allowed_in_embed".into()),
        ));
        return;
    }
    if mode == ImageAssetMode::Embed {
        let Some(bytes) = attachment.data.as_deref() else {
            doc.image_assets.push(result(
                source,
                ImageAssetStatus::Failed,
                Some("missing_bytes".into()),
            ));
            return;
        };
        match pagesift_base64::encode_data_url(attachment.mime(), bytes) {
            Ok(uri) => {
                replacements.insert(dest, uri);
            }
            Err(_) => {
                doc.image_assets.push(result(
                    source,
                    ImageAssetStatus::Failed,
                    Some("encoded_length_overflow".into()),
                ));
                return;
            }
        }
    } else if mode == ImageAssetMode::Rewrite {
        replacements.insert(dest, format!("asset://{}", attachment.id));
    }
    doc.image_assets.push(success(source, &attachment));
    if mode != ImageAssetMode::Embed && seen.insert(attachment.hash.clone()) {
        attachment.inline_data = true;
        doc.attachments.push(attachment);
    }
}

impl Scraper {
    async fn fetch_asset(
        &self,
        url: &str,
        robots: RobotsMode,
        budget: Arc<pagesift_net::SharedBodyBudget>,
        cap: usize,
        deadline: tokio::time::Instant,
    ) -> Result<Attachment, (ImageAssetStatus, String, usize)> {
        let mut req = FetchRequest::new(url);
        req.max_body_bytes = Some(cap);
        req.shared_body_budget = Some(budget);
        let response = match tokio::time::timeout_at(deadline, self.policy.fetch(req, robots)).await
        {
            Err(_) => return Err((ImageAssetStatus::SkippedLimit, "image_time_limit".into(), 0)),
            Ok(Ok(resp)) if resp.truncated => {
                return Err((
                    ImageAssetStatus::SkippedLimit,
                    "image_exceeds_budget".into(),
                    resp.body.len(),
                ));
            }
            Ok(Ok(resp)) if !(200..300).contains(&resp.status) => {
                return Err((
                    ImageAssetStatus::Failed,
                    format!("http_status_{}", resp.status),
                    resp.body.len(),
                ));
            }
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                let code = err.code();
                let status = if code == ErrorCode::TooLarge {
                    ImageAssetStatus::SkippedLimit
                } else {
                    ImageAssetStatus::Failed
                };
                return Err((status, code.as_str().into(), 0));
            }
        };
        let format =
            pagesift_api::types::attachment::sniff_image_format(&response.body).ok_or((
                ImageAssetStatus::Failed,
                "unrecognized_image_bytes".into(),
                response.body.len(),
            ))?;
        Ok(Attachment::new(response.body, format, Some(url.into())))
    }

    pub(super) async fn assetize_images(
        &self,
        doc: &mut Document,
        robots: RobotsMode,
        mode: ImageAssetMode,
        cache: Option<&pagesift_api::types::AssetFetchCache>,
    ) {
        let mut replacements = HashMap::new();
        let mut seen = HashSet::new();
        // PDF and Office exporters already return bytes through Attachment.
        for attachment in std::mem::take(&mut doc.attachments) {
            let dest = attachment
                .source
                .clone()
                .unwrap_or_else(|| format!("images/{}", attachment.id));
            add_asset(
                doc,
                mode,
                dest.clone(),
                dest,
                attachment,
                &mut replacements,
                &mut seen,
            );
        }
        let mut destinations = HashSet::new();
        if let Some(markdown) = &doc.markdown {
            image_destinations(markdown, |span| {
                destinations.insert(markdown[span].to_string());
            });
        }
        let mut urls = HashSet::new();
        let deadline = tokio::time::Instant::now() + IMAGE_DEADLINE;
        let cap = pagesift_net::resources::body_cap(self.policy.fetcher().config().max_body_bytes);
        let budget = Arc::new(pagesift_net::SharedBodyBudget::new(cap));
        let mut pending = Vec::new();
        for url in doc.images.clone().unwrap_or_default() {
            let dest = markdown_url(&url);
            if !destinations.contains(&dest)
                || !urls.insert(url.clone())
                || !(url.starts_with("http://") || url.starts_with("https://"))
            {
                continue;
            }
            if pending.len() >= MAX_IMAGE_REQUESTS {
                doc.image_assets.push(result(
                    url,
                    ImageAssetStatus::SkippedLimit,
                    Some("image_request_limit".into()),
                ));
                continue;
            }
            pending.push((url, dest));
        }
        let mut fetched = stream::iter(pending)
            .map(|(url, dest)| {
                let budget = budget.clone();
                async move {
                    let fetched_here = std::sync::atomic::AtomicBool::new(false);
                    let fetched = if budget.remaining() == 0 {
                        Err((
                            ImageAssetStatus::SkippedLimit,
                            "image_budget_exhausted".into(),
                            0,
                        ))
                    } else if let Some((cache, cell)) =
                        cache.and_then(|c| c.entry(&url).map(|cell| (c, cell)))
                    {
                        let fetched = tokio::time::timeout_at(
                            deadline,
                            cell.get_or_try_init(|| {
                                fetched_here.store(true, std::sync::atomic::Ordering::Relaxed);
                                self.fetch_asset(&url, robots, budget.clone(), cap, deadline)
                            }),
                        )
                        .await;
                        match fetched {
                            Ok(Ok(asset)) => {
                                let asset = asset.clone();
                                if asset.bytes > 512 * 1024 {
                                    cache.discard(&url, &cell);
                                }
                                Ok(asset)
                            }
                            Ok(Err(e)) => {
                                cache.discard(&url, &cell);
                                Err(e)
                            }
                            Err(_) => {
                                Err((ImageAssetStatus::SkippedLimit, "image_time_limit".into(), 0))
                            }
                        }
                    } else {
                        fetched_here.store(true, std::sync::atomic::Ordering::Relaxed);
                        self.fetch_asset(&url, robots, budget.clone(), cap, deadline)
                            .await
                    };
                    let fetched = match fetched {
                        Ok(a) if !fetched_here.load(std::sync::atomic::Ordering::Relaxed) => {
                            let size = a.bytes as usize;
                            let reserved = budget.take(size);
                            if reserved == size {
                                Ok(a)
                            } else {
                                budget.give_back(reserved);
                                Err((
                                    ImageAssetStatus::SkippedLimit,
                                    "image_exceeds_budget".into(),
                                    0,
                                ))
                            }
                        }
                        other => other,
                    };
                    (url, dest, fetched)
                }
            })
            .buffered(IMAGE_CONCURRENCY);
        while let Some((url, dest, fetched_result)) = fetched.next().await {
            let attachment = match fetched_result {
                Ok(a) => a,
                Err((status, reason, _)) => {
                    doc.image_assets.push(result(url, status, Some(reason)));
                    continue;
                }
            };
            add_asset(
                doc,
                mode,
                url,
                dest,
                attachment,
                &mut replacements,
                &mut seen,
            );
        }
        if !replacements.is_empty() {
            if let Some(markdown) = doc.markdown.as_mut() {
                *markdown = rewrite(markdown, &replacements);
            }
            if let Some(pages) = doc.pages.as_mut() {
                for page in pages {
                    page.markdown = rewrite(&page.markdown, &replacements);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, response::Html, routing::get};
    use pagesift_api::types::ScrapeOptions;
    use serde_json::json;

    fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = out.len() as u32;
            out.extend_from_slice(&0x04034b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0; 2]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&0x02014b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&[0; 8]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x06054b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        out
    }

    fn pdf_with_image() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 20 90 Td (Text layer keeps OCR independent.) Tj ET q 20 0 0 20 20 20 cm /Im1 Do Q";
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 6 0 R >> >> /Contents 5 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            [format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(), content, b"\nendstream"].concat(),
            [b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\n".as_slice(), &[255, 0, 0], b"\nendstream"].concat(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(object);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        out
    }

    #[tokio::test]
    async fn document_assets_use_exported_bytes_without_ocr() {
        use pagesift_api::types::{Upload, UploadKind};
        let scraper = Scraper::new(super::super::ScraperConfig::default()).unwrap();
        let opts: ScrapeOptions = serde_json::from_value(json!({"imageAssets":"rewrite"})).unwrap();
        let upload = Upload {
            bytes: pdf_with_image(),
            filename: "test.pdf".into(),
            content_type: None,
            kind: UploadKind::Pdf,
            url: None,
        };
        let pdf = scraper.parse(upload, &opts).await.unwrap();
        assert_eq!(pdf.attachments.len(), 1, "{:?}", pdf.image_assets);
        let asset = &pdf.attachments[0];
        let calculated =
            Attachment::new(asset.data.as_ref().unwrap().to_vec(), &asset.format, None);
        assert_eq!(asset.hash, calculated.hash);
        assert!(
            pdf.markdown
                .as_deref()
                .unwrap()
                .contains(&format!("asset://{}", asset.id))
        );
        assert_eq!(pdf.image_assets[0].status, ImageAssetStatus::Ok);

        let doc = br#"<w:document><w:body><w:p><w:r><w:t>Text</w:t></w:r><w:r><w:drawing><a:blip r:embed="i1"/></w:drawing></w:r></w:p></w:body></w:document>"#;
        let rels = br#"<Relationships><Relationship Id="i1" Type="x/image" Target="media/a.jpg"/></Relationships>"#;
        let png = b"\x89PNG\r\n\x1a\nimage";
        let bytes = stored_zip(&[
            ("word/document.xml", doc),
            ("word/_rels/document.xml.rels", rels),
            ("word/media/a.jpg", png),
        ]);
        let upload = Upload {
            bytes,
            filename: "test.docx".into(),
            content_type: None,
            kind: UploadKind::Office(pagesift_office::Format::Docx),
            url: None,
        };
        let doc = scraper.parse(upload, &opts).await.unwrap();
        assert_eq!(doc.attachments.len(), 1, "{:?}", doc.image_assets);
        let a = &doc.attachments[0];
        assert_eq!(a.mime(), "image/png");
        assert_eq!(a.data.as_deref(), Some(png.as_slice()));
        assert!(
            doc.markdown
                .as_deref()
                .unwrap()
                .contains(&format!("asset://{}", a.id))
        );
    }

    #[tokio::test]
    async fn powerpoint_and_spreadsheet_media_are_assets() {
        use pagesift_api::types::{Upload, UploadKind};
        let png = b"\x89PNG\r\n\x1a\nimage";
        let presentation = br#"<p:presentation><p:sldIdLst><p:sldId id="256" r:id="s1"/></p:sldIdLst></p:presentation>"#;
        let pres_rels = br#"<Relationships><Relationship Id="s1" Type="x/slide" Target="slides/slide1.xml"/></Relationships>"#;
        let slide =
            br#"<p:sld><p:pic><p:blipFill><a:blip r:embed="i"/></p:blipFill></p:pic></p:sld>"#;
        let slide_rels = br#"<Relationships><Relationship Id="i" Type="x/image" Target="../media/logo.png"/></Relationships>"#;
        let pptx = stored_zip(&[
            ("ppt/presentation.xml", presentation),
            ("ppt/_rels/presentation.xml.rels", pres_rels),
            ("ppt/slides/slide1.xml", slide),
            ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
            ("ppt/media/logo.png", png),
        ]);
        let workbook =
            br#"<workbook><sheets><sheet name="Data" sheetId="1" r:id="w1"/></sheets></workbook>"#;
        let workbook_rels = br#"<Relationships><Relationship Id="w1" Type="x/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let xlsx = stored_zip(&[
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/worksheets/sheet1.xml", b"<worksheet/>"),
            ("xl/media/logo.png", png),
        ]);
        let scraper = Scraper::new(super::super::ScraperConfig::default()).unwrap();
        let opts: ScrapeOptions = serde_json::from_value(json!({"imageAssets":"attach"})).unwrap();
        for (name, format, bytes) in [
            ("test.pptx", pagesift_office::Format::Pptx, pptx),
            ("test.xlsx", pagesift_office::Format::Xlsx, xlsx),
        ] {
            let upload = Upload {
                bytes,
                filename: name.into(),
                content_type: None,
                kind: UploadKind::Office(format),
                url: None,
            };
            let doc = scraper.parse(upload, &opts).await.unwrap();
            assert_eq!(doc.attachments.len(), 1, "{name}: {:?}", doc.image_assets);
            assert_eq!(doc.attachments[0].data.as_deref(), Some(png.as_slice()));
            assert_eq!(doc.image_assets[0].status, ImageAssetStatus::Ok);
        }
    }

    #[test]
    fn only_image_destinations_outside_code_are_rewritten() {
        let text = "![a](https://e/x.png \"title\") [link](https://e/x.png) `![x](https://e/x.png)`\n```\n![x](https://e/x.png)\n```\n";
        let replacements = HashMap::from([("https://e/x.png".into(), "asset://abc".into())]);
        assert_eq!(
            rewrite(text, &replacements),
            "![a](asset://abc \"title\") [link](https://e/x.png) `![x](https://e/x.png)`\n```\n![x](https://e/x.png)\n```\n"
        );
    }

    #[tokio::test]
    async fn task_cache_coalesces_concurrent_image_downloads() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let count = hits.clone();
        let app = Router::new()
            .route(
                "/",
                get(|| async { Html("<p>Picture</p><img alt='one' src='/a.png'>") }),
            )
            .route(
                "/a.png",
                get(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                        &b"\x89PNG\r\n\x1a\nimage"[..]
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = super::super::ScraperConfig::default();
        config.fetcher.env_proxy = false;
        config.fetcher.ssrf.allow_private_network = true;
        config.policy.rate.default_interval = std::time::Duration::ZERO;
        let scraper = Scraper::new(config).unwrap();
        let mut opts: ScrapeOptions =
            serde_json::from_value(json!({"engine":"http", "imageAssets":"rewrite"})).unwrap();
        opts.asset_fetch_cache = Some(Default::default());
        let (a, b) = tokio::join!(scraper.scrape(&url, &opts), scraper.scrape(&url, &opts));
        assert_eq!(a.unwrap().attachments.len(), 1);
        assert_eq!(b.unwrap().attachments.len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        opts.asset_fetch_cache = Some(Default::default());
        scraper.scrape(&url, &opts).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn image_fetches_run_concurrently_with_one_shared_budget() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let started = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let count = started.clone();
        let wait = gate.clone();
        let site = Router::new()
            .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
            .route(
                "/",
                get(|| async {
                    Html("<p>Pictures</p><img alt='a' src='/a.png'><img alt='b' src='/b.png'>")
                }),
            )
            .route(
                "/{name}",
                get(move || {
                    let count = count.clone();
                    let wait = wait.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        wait.acquire().await.unwrap().forget();
                        let mut image = b"\x89PNG\r\n\x1a\n".to_vec();
                        image.resize(3000, 42);
                        image
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, site).await.unwrap() });
        let mut config = super::super::ScraperConfig::default();
        config.fetcher.env_proxy = false;
        config.fetcher.ssrf.allow_private_network = true;
        config.fetcher.adaptive_host_connections = false;
        config.fetcher.max_body_bytes = 4096;
        config.policy.rate.default_interval = std::time::Duration::ZERO;
        config.policy.rate.per_host_concurrency = 4;
        let scraper = Scraper::new(config).unwrap();
        let opts: ScrapeOptions =
            serde_json::from_value(json!({"engine":"http", "imageAssets":"attach"})).unwrap();
        let work = scraper.scrape(&url, &opts);
        let drive = async {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while started.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            gate.add_permits(2);
        };
        let (doc, _) = tokio::join!(work, drive);
        let doc = doc.unwrap();
        assert_eq!(doc.image_assets.len(), 2);
        assert!(
            doc.image_assets
                .iter()
                .any(|a| a.status == ImageAssetStatus::SkippedLimit)
        );
        assert!(doc.attachments.iter().map(|a| a.bytes).sum::<u64>() <= 4096);
    }

    #[tokio::test]
    async fn shared_budget_counts_gzip_expansion() {
        use std::{io::Write, sync::Arc};
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.resize(1000, 7);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&png).unwrap();
        let compressed = Arc::new(encoder.finish().unwrap());
        let data = compressed.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let data = data.clone();
                async move { ([("content-encoding", "gzip")], data.as_ref().clone()) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = super::super::ScraperConfig::default();
        config.fetcher.env_proxy = false;
        config.fetcher.ssrf.allow_private_network = true;
        config.policy.rate.default_interval = std::time::Duration::ZERO;
        let scraper = Scraper::new(config).unwrap();
        // wall-clock: the local server answers immediately; this deadline is far above that and
        // bounds a hung test, while the asserted byte budgets do not depend on elapsed time.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let budget = Arc::new(pagesift_net::SharedBodyBudget::new(1200));
        let a = scraper
            .fetch_asset(&url, RobotsMode::Ignore, budget.clone(), 1200, deadline)
            .await
            .unwrap();
        assert_eq!(a.data.as_deref(), Some(png.as_slice()));
        assert_eq!(budget.remaining(), 200);
        let tight = Arc::new(pagesift_net::SharedBodyBudget::new(500));
        let err = scraper
            .fetch_asset(&url, RobotsMode::Ignore, tight, 500, deadline)
            .await
            .unwrap_err();
        assert_eq!(err.0, ImageAssetStatus::SkippedLimit);
    }

    #[tokio::test]
    async fn stream_emits_markdown_before_image_fetch_finishes() {
        use axum::{body::Body, http::Request};
        use http_body_util::BodyExt;
        use std::sync::Arc;
        use tower::ServiceExt;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let wait = gate.clone();
        let site = Router::new()
            .route(
                "/",
                get(|| async { Html("<p>Early body text.</p><img alt='one' src='/a.png'>") }),
            )
            .route(
                "/a.png",
                get(move || {
                    let wait = wait.clone();
                    async move {
                        let _permit = wait.acquire().await.unwrap();
                        &b"\x89PNG\r\n\x1a\nimage"[..]
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, site).await.unwrap() });
        let mut config = super::super::ScraperConfig::default();
        config.fetcher.env_proxy = false;
        config.fetcher.ssrf.allow_private_network = true;
        config.policy.rate.default_interval = std::time::Duration::ZERO;
        let engine = Arc::new(Scraper::new(config).unwrap());
        let app = pagesift_api::router(engine, pagesift_api::Config::default());
        let request = Request::post("/v2/scrape/stream")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"url":url,"engine":"http","imageAssets":"rewrite"}).to_string(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let mut body = response.into_body();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(3), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
        assert!(text.contains("event: markdown"), "{text}");
        assert!(text.contains("Early body text"), "{text}");
        gate.add_permits(1);
        let final_bytes = tokio::time::timeout(std::time::Duration::from_secs(3), body.collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        let text = String::from_utf8(final_bytes.to_vec()).unwrap();
        assert!(text.contains("event: done"), "{text}");
        assert!(text.contains("asset://"), "{text}");
    }

    #[tokio::test]
    async fn crawl_stream_can_emit_previews_before_images_finish() {
        use axum::{body::Body, http::Request};
        use http_body_util::BodyExt;
        use std::sync::Arc;
        use tower::ServiceExt;
        let page_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let image_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let pg = page_gate.clone();
        let ig = image_gate.clone();
        let site = Router::new()
            .route("/robots.txt", get(|| async { "User-agent: *\nAllow: /\n" }))
            .route(
                "/",
                get(move || {
                    let pg = pg.clone();
                    async move {
                        pg.acquire().await.unwrap().forget();
                        Html("<p>Crawl early text.</p><img alt='a' src='/a.png'>")
                    }
                }),
            )
            .route(
                "/a.png",
                get(move || {
                    let ig = ig.clone();
                    async move {
                        ig.acquire().await.unwrap().forget();
                        &b"\x89PNG\r\n\x1a\nimage"[..]
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, site).await.unwrap() });
        let mut config = super::super::ScraperConfig::default();
        config.fetcher.env_proxy = false;
        config.fetcher.ssrf.allow_private_network = true;
        config.policy.rate.default_interval = std::time::Duration::ZERO;
        let engine = Arc::new(Scraper::new(config).unwrap());
        let app = pagesift_api::router(engine, pagesift_api::Config::default());
        let start = Request::post("/v2/crawl").header("content-type", "application/json")
            .body(Body::from(json!({"url":url,"limit":1,"sitemap":"skip","scrapeOptions":{"engine":"http","imageAssets":"rewrite"}}).to_string())).unwrap();
        let response = app.clone().oneshot(start).await.unwrap();
        let data: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let id = data["id"].as_str().unwrap();
        let request = Request::get(format!("/v2/crawl/{id}/stream?previews=true"))
            .body(Body::empty())
            .unwrap();
        let mut body = app.oneshot(request).await.unwrap().into_body();
        page_gate.add_permits(1);
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
        assert!(text.contains("event: markdown"), "{text}");
        assert!(text.contains("Crawl early text"), "{text}");
        image_gate.add_permits(1);
        let rest = tokio::time::timeout(std::time::Duration::from_secs(5), body.collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        let rest = String::from_utf8(rest.to_vec()).unwrap();
        assert!(rest.contains("event: page"), "{rest}");
        assert!(rest.contains("asset://"), "{rest}");
    }

    #[tokio::test]
    async fn modes_fetch_images_and_never_embed_svg() {
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage";
        let app = Router::new()
            .route("/", get(|| async { Html(r#"<html><body><p>Picture test page.</p><img alt="one" src="/a.png"><img alt="two" src="/b.png"><img alt="svg" src="/x.svg"><img alt="bad" src="/bad.png"></body></html>"#) }))
            .route("/a.png", get(|| async { PNG }))
            .route("/b.png", get(|| async { PNG }))
            .route("/x.svg", get(|| async { "<svg xmlns=\"http://www.w3.org/2000/svg\"/>" }))
            .route("/bad.png", get(|| async { ([ ("content-type", "image/png") ], "not an image") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = super::super::ScraperConfig::default();
        config.fetcher.env_proxy = false;
        config.fetcher.ssrf.allow_private_network = true;
        config.policy.rate.default_interval = std::time::Duration::ZERO;
        let scraper = Scraper::new(config).unwrap();
        for mode in ["attach", "embed", "rewrite"] {
            let opts: ScrapeOptions =
                serde_json::from_value(json!({"engine":"http", "imageAssets": mode})).unwrap();
            let doc = scraper.scrape(&url, &opts).await.unwrap();
            let md = doc.markdown.as_deref().unwrap();
            assert_eq!(doc.image_assets.len(), 4, "{mode}: {:?}", doc.image_assets);
            let a = doc
                .image_assets
                .iter()
                .find(|a| a.source.ends_with("/a.png"))
                .unwrap();
            let b = doc
                .image_assets
                .iter()
                .find(|a| a.source.ends_with("/b.png"))
                .unwrap();
            assert_eq!(a.id, b.id);
            let bad = doc
                .image_assets
                .iter()
                .find(|a| a.source.ends_with("/bad.png"))
                .unwrap();
            assert_eq!(bad.status, ImageAssetStatus::Failed);
            let svg = doc
                .image_assets
                .iter()
                .find(|a| a.source.ends_with("/x.svg"))
                .unwrap();
            if mode == "embed" {
                assert!(md.contains("data:image/png;base64,"));
                assert!(!md.contains("data:image/svg+xml"));
                assert_eq!(svg.status, ImageAssetStatus::Failed);
                assert!(doc.attachments.is_empty());
            } else {
                assert_eq!(doc.attachments.len(), 2);
                assert!(doc.attachments.iter().all(|a| a.inline_data));
                assert_eq!(svg.status, ImageAssetStatus::Ok);
                if mode == "rewrite" {
                    assert!(md.contains(&format!("asset://{}", a.id.as_ref().unwrap())));
                } else {
                    assert!(md.contains("/a.png"));
                }
            }
        }
    }
}
