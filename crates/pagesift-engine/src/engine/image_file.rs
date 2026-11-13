//! 图片文件 → 文档：解成灰度后交给和扫描 PDF 相同的 OCR 引擎。OCR 要服务端先打开
//! （`[ocr] enabled`），没开时报 UNSUPPORTED_OPTION 并说明怎么开，不返回空正文。

use super::*;

/// [策略] 单张图的像素上限：约 16 个 A4 600 dpi 的页面。超过的多半是炸弹或航拍图，OCR 也跑不动。
const MAX_IMAGE_PIXELS: u64 = 500_000_000;

/// 没开 OCR 时的说明，抓取和上传共用。
pub(super) const OCR_OFF: &str =
    "images need OCR turned on on this server: [ocr] enabled = true or --ocr";

impl Scraper {
    pub(super) async fn image_document(
        &self,
        bytes: Vec<u8>,
        opts: &ScrapeOptions,
        (meta, mut ext): (Metadata, PagesiftMeta),
        mut p: Progress,
        deadline: Option<Instant>,
    ) -> Result<Document, ApiError> {
        let Some(ocr) = self.ocr.clone() else {
            return Err(ApiError::unsupported(OCR_OFF));
        };
        let stage = self
            .policy
            .fetcher()
            .acquire_stage(pagesift_net::NetworkStage::Parse)
            .await;
        let t = Instant::now();
        let engine = ocr.clone();
        let result = cpu_task(move || {
            ocr.prepare(deadline)?;
            let image = pagesift_pdf::decode_image_file(&bytes, MAX_IMAGE_PIXELS)?;
            drop(bytes);
            ocr.recognize(image.width, image.height, &image.pixels, deadline)
        })
        .await?;
        let output = match result {
            Ok(output) => {
                stage.finish(pagesift_net::StageOutcome::Success);
                output
            }
            Err(message) => {
                stage.finish(pagesift_net::StageOutcome::DeterministicFailure);
                return Err(ApiError::unsupported(format!(
                    "could not read image: {message}"
                )));
            }
        };
        p.timings.insert("ocr".into(), ms(t.elapsed()));
        if let Some(reason) = &output.fallback {
            p.warnings.push(format!("ocr_fallback: {reason}"));
        }
        if let Some(reason) = &output.partial_error {
            p.warnings.push(format!("ocr_partial: {reason}"));
        }
        let markdown = lines_to_markdown(&output.lines);
        if markdown.is_empty() {
            p.warnings
                .push("ocr_empty: no text was recognized in this image".into());
        }
        let info = engine.info();
        ext.ocr = Some(pagesift_api::types::OcrMeta {
            engine: info.engine,
            model: info.model,
            device: output.device.clone().unwrap_or(info.device),
            pages: 1,
            fallback: output.fallback.is_some(),
        });
        let graph_opts = crate::graph::effective_graph_options(opts);
        let content_hash = pagesift_graph::sha256_hex(markdown.as_bytes());
        if ext.content_hash.is_none() {
            ext.content_hash = Some(content_hash.clone());
        }
        let is_upload = ext.engine.as_deref() == Some("upload");
        let parent_url = meta
            .url
            .clone()
            .or_else(|| meta.source_url.clone())
            .unwrap_or_default();
        let mut out = finish(
            Document {
                markdown: (opts.wants("markdown") || graph_opts.is_some()).then_some(markdown),
                ..Default::default()
            },
            meta,
            ext,
            p.timings,
            p.warnings,
            None,
            false,
            p.started,
        );
        if let Some(gopts) = &graph_opts {
            let store = self.store.as_ref().filter(|_| !is_upload);
            crate::graph::build_and_attach_page_graph(
                store,
                gopts,
                &mut out,
                crate::graph::PageGraphSeed {
                    source_kind: pagesift_graph::SourceKind::ImageAsset {
                        parent_url,
                        media_id: content_hash.clone(),
                    },
                    engine: "ocr".to_string(),
                    content_hash: Some(content_hash),
                    ..Default::default()
                },
            );
            if !opts.wants("markdown") {
                out.markdown = None;
            }
        }
        Ok(out)
    }
}

/// 识别结果按阅读顺序拼：高度重叠超过一半的算同一行，从左到右；上下间隔超过行高一半就分段。
/// 公式行单独成段。
fn lines_to_markdown(lines: &[pagesift_pdf::OcrLine]) -> String {
    let mut sorted: Vec<&pagesift_pdf::OcrLine> =
        lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    sorted.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let mut rows: Vec<Vec<&pagesift_pdf::OcrLine>> = Vec::new();
    for line in sorted {
        let h = (line.y1 - line.y0).max(1.0);
        let same = rows.last().is_some_and(|r: &Vec<_>| {
            let last = r[r.len() - 1];
            !line.formula && !last.formula && (last.y1.min(line.y1) - line.y0) > h * 0.5
        });
        if same {
            if let Some(r) = rows.last_mut() {
                r.push(line);
            }
        } else {
            rows.push(vec![line]);
        }
    }
    let mut out = String::new();
    let mut prev_bottom: Option<f32> = None;
    let mut prev_formula = false;
    for row in &mut rows {
        row.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let top = row.iter().map(|l| l.y0).fold(f32::MAX, f32::min);
        let bottom = row.iter().map(|l| l.y1).fold(f32::MIN, f32::max);
        let height = (bottom - top).max(1.0);
        let formula = row[0].formula;
        if let Some(prev) = prev_bottom {
            let new_para = formula || prev_formula || top - prev > height * 0.5;
            out.push_str(if new_para { "\n\n" } else { "\n" });
        }
        let text: Vec<&str> = row.iter().map(|l| l.text.trim()).collect();
        out.push_str(&text.join(" "));
        prev_bottom = Some(bottom);
        prev_formula = formula;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x0: f32, y0: f32, y1: f32, text: &str, formula: bool) -> pagesift_pdf::OcrLine {
        pagesift_pdf::OcrLine {
            x0,
            y0,
            x1: x0 + 10.0,
            y1,
            text: text.into(),
            score: 1.0,
            formula,
        }
    }

    #[test]
    fn reading_order_rows_and_paragraphs() {
        let lines = [
            line(50.0, 0.0, 10.0, "right", false),
            line(0.0, 1.0, 11.0, "left", false),
            line(0.0, 12.0, 22.0, "next", false),
            line(0.0, 60.0, 70.0, "para", false),
            line(0.0, 72.0, 82.0, "$$x$$", true),
            line(0.0, 84.0, 94.0, " ", false),
        ];
        assert_eq!(
            lines_to_markdown(&lines),
            "left right\nnext\n\npara\n\n$$x$$\n"
        );
        assert_eq!(lines_to_markdown(&[]), "");
    }
}
