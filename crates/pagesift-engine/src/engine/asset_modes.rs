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

/// Strip up to 3 leading spaces and optional blockquote `>` markers (CommonMark §4.5).
fn strip_fence_prefix(mut s: &str) -> Option<&str> {
    // [协议] CommonMark §4.5 代码围栏前最多允许 3 个缩进空格；>= 4 空格属于缩进代码块
    const MAX_FENCE_INDENT: usize = 3;
    loop {
        let spaces = s.bytes().take_while(|&b| b == b' ').count();
        if spaces > MAX_FENCE_INDENT || s.as_bytes().get(spaces) == Some(&b'\t') {
            return None;
        }
        s = &s[spaces..];
        if let Some(rest) = s.strip_prefix('>') {
            s = rest.strip_prefix(' ').unwrap_or(rest);
        } else {
            return Some(s);
        }
    }
}

/// Find the end offset (right after the closing backtick run) of a matching inline code span of
/// exact length `run_len` on the same line, or `None` if unclosed (CommonMark §6.1).
fn find_matching_backtick_run(bytes: &[u8], mut pos: usize, run_len: usize) -> Option<usize> {
    while pos < bytes.len() {
        if bytes[pos] == b'`' {
            let n = bytes[pos..].iter().take_while(|&&b| b == b'`').count();
            if n == run_len {
                return Some(pos + n);
            }
            pos += n;
        } else {
            pos += 1;
        }
    }
    None
}

/// Visit image destinations in Markdown emitted by the content/PDF/Office
/// renderers in strict O(N) time. Fenced and inline code are ignored; generated
/// destinations have parentheses and spaces percent-encoded and may be followed
/// by a title.
fn image_destinations(markdown: &str, mut visit: impl FnMut(std::ops::Range<usize>)) {
    // [协议] CommonMark §4.5 代码围栏最少由 3 个连续反引号或波浪号组成
    const MIN_FENCE_LEN: usize = 3;
    // [策略] 位掩码记录本行已知不存在配对闭合的反引号串长度（1..64），保证海量孤立反引号下仍严格 O(L)
    const TRACKED_TICK_RUNS: usize = 64;
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;
    let mut bracket_is_image: Vec<bool> = Vec::new();
    for line in markdown.split_inclusive('\n') {
        let bytes = line.as_bytes();
        if let Some(trimmed) = strip_fence_prefix(line)
            && let Some(marker @ (b'`' | b'~')) = trimmed.as_bytes().first().copied()
        {
            let count = trimmed
                .as_bytes()
                .iter()
                .take_while(|&&b| b == marker)
                .count();
            if count >= MIN_FENCE_LEN {
                let tail = trimmed[count..].trim_end_matches(['\r', '\n']);
                match fence {
                    // CommonMark §4.5: backtick info strings cannot contain backticks;
                    // ` ```inline``` ![img](...) ` is inline code, not a fenced block.
                    None if marker != b'`' || !tail.as_bytes().contains(&b'`') => {
                        fence = Some((marker, count));
                        offset += line.len();
                        continue;
                    }
                    None => {}
                    // Closing fence allows only optional trailing spaces/tabs.
                    Some((kind, n))
                        if kind == marker
                            && count >= n
                            && tail.bytes().all(|b| b == b' ' || b == b'\t') =>
                    {
                        fence = None;
                        offset += line.len();
                        continue;
                    }
                    Some(_) => {
                        offset += line.len();
                        continue;
                    }
                }
            }
        }
        if fence.is_some() {
            offset += line.len();
            continue;
        }
        bracket_is_image.clear();
        let mut unclosed_tick_mask = 0u64;
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                if bytes.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if bytes[i] == b'`' {
                let count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                let bit = if count < TRACKED_TICK_RUNS {
                    1u64 << count
                } else {
                    0
                };
                if (bit == 0 || (unclosed_tick_mask & bit) == 0)
                    && let Some(after_span) = find_matching_backtick_run(bytes, i + count, count)
                {
                    i = after_span;
                    continue;
                }
                unclosed_tick_mask |= bit;
                i += count;
                continue;
            }
            if bytes[i..].starts_with(b"![") {
                bracket_is_image.push(true);
                i += 2;
                continue;
            }
            if bytes[i] == b'[' {
                if !bracket_is_image.is_empty() {
                    bracket_is_image.push(false);
                }
                i += 1;
                continue;
            }
            if bytes[i] == b']' {
                if let Some(true) = bracket_is_image.pop()
                    && bytes.get(i + 1) == Some(&b'(')
                {
                    let start = i + 2;
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
                    i = start;
                    continue;
                }
                i += 1;
                continue;
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
mod tests;
