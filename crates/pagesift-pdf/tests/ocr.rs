//! 扫描页 OCR 的接入：用假引擎看哪些图交给引擎、结果怎么合并、文字层什么时候清、错误和时限怎么报。

mod common;

use std::panic::panic_any;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{Builder, Page, line, save_raw};
use lopdf::{Object, dictionary};
use pagesift_pdf::{Error, OcrLine, OcrOutput, Options, PageOcr, extract_with, parse_timeout};

type Respond = dyn Fn(usize) -> Result<OcrOutput, String> + Send + Sync;

/// 第 n 次调用（从 0 起）的结果由测试给。
struct Script {
    calls: AtomicUsize,
    deadlines: Mutex<Vec<Option<Instant>>>,
    not_ready: Option<String>,
    respond: Box<Respond>,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Script")
    }
}

impl PageOcr for Script {
    fn prepare(&self, _deadline: Option<Instant>) -> Result<(), String> {
        match &self.not_ready {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn recognize(
        &self,
        _w: u32,
        _h: u32,
        _gray: &[u8],
        deadline: Option<Instant>,
    ) -> Result<OcrOutput, String> {
        self.deadlines.lock().unwrap().push(deadline);
        (self.respond)(self.calls.fetch_add(1, Ordering::SeqCst))
    }
}

fn script(
    respond: impl Fn(usize) -> Result<OcrOutput, String> + Send + Sync + 'static,
) -> std::sync::Arc<Script> {
    std::sync::Arc::new(Script {
        calls: AtomicUsize::new(0),
        deadlines: Mutex::new(Vec::new()),
        not_ready: None,
        respond: Box::new(respond),
    })
}

/// 图上一行字（测试图 2×2 像素），`y`、`height` 是图上的像素坐标。
fn ocr_line(text: &str, y: f32, height: f32) -> OcrLine {
    OcrLine {
        x0: 0.0,
        y0: y,
        x1: 2.0,
        y1: y + height,
        text: text.into(),
        score: 0.9,
        formula: false,
    }
}

fn says(text: &str) -> Result<OcrOutput, String> {
    Ok(OcrOutput {
        lines: vec![ocr_line(text, 0.5, 0.1)],
        ..Default::default()
    })
}

/// 把图 /Im1 画到 (x, y)、大小 w×h。
fn draw(x: f64, y: f64, w: f64, h: f64) -> String {
    format!("q {w} 0 0 {h} {x} {y} cm /Im1 Do Q\n")
}

fn pdf(content: String) -> Vec<u8> {
    Builder::new(vec![Page::text(content)]).build()
}

/// 足够长的文字层，不算扫描页。
fn body() -> String {
    line(
        "F1",
        12.0,
        72.0,
        700.0,
        "The body text of this page stays in the document whatever the logo says.",
    )
}

fn options(engine: &std::sync::Arc<Script>, force_ocr: bool) -> Options {
    Options {
        ocr: Some(engine.clone()),
        force_ocr,
        ..Default::default()
    }
}

/// `mode: ocr`：小 logo 上认出字不能丢掉整页文字层，认出的字加在后面。
#[test]
fn a_logo_does_not_clear_the_text_layer() {
    let engine = script(|_| says("ACME"));
    let bytes = pdf(format!("{}{}", draw(500.0, 720.0, 50.0, 50.0), body()));
    let doc = extract_with(&bytes, &options(&engine, true)).unwrap();
    assert!(doc.markdown.contains("The body text"), "{}", doc.markdown);
    assert!(doc.markdown.contains("ACME"), "{}", doc.markdown);
    assert!(doc.pages[0].ocr);
}

/// `mode: ocr`：认出字的图盖住了足够大的一块（文字层被认为不可信）才换掉文字层。
#[test]
fn a_large_image_replaces_the_text_layer() {
    let engine = script(|_| says("Recognized"));
    let bytes = pdf(format!("{}{}", draw(0.0, 0.0, 612.0, 396.0), body()));
    let doc = extract_with(&bytes, &options(&engine, true)).unwrap();
    assert!(!doc.markdown.contains("The body text"), "{}", doc.markdown);
    assert!(doc.markdown.contains("Recognized"), "{}", doc.markdown);
}

/// 一页由两块图拼成：都要认，结果合起来；字号按全页取，不因为两块的行高不同变成标题。
#[test]
fn tiles_are_merged_with_one_font_size() {
    let engine = script(|n| {
        let height = if n == 0 { 0.06 } else { 0.1 };
        Ok(OcrOutput {
            lines: (0..5)
                .map(|i| {
                    let text = format!("tile {n} line {i} of the scanned page");
                    ocr_line(&text, 0.1 + 0.3 * i as f32, height)
                })
                .collect(),
            ..Default::default()
        })
    });
    let bytes = pdf(format!(
        "{}{}",
        draw(0.0, 396.0, 612.0, 396.0),
        draw(0.0, 0.0, 612.0, 396.0)
    ));
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    assert!(doc.markdown.contains("tile 0 line 0"), "{}", doc.markdown);
    assert!(doc.markdown.contains("tile 1 line 4"), "{}", doc.markdown);
    assert!(!doc.markdown.contains('#'), "{}", doc.markdown);
    assert!(doc.pages[0].ocr);
    // 没要图片列表时仍然只列说明要引用的那张图
    assert_eq!(doc.images.len(), 1);
}

/// 分层扫描（MRC）的几层都是整页图：认出字后别的层不再认，否则文字重复、时间翻倍。
#[test]
fn layers_of_one_page_are_read_once() {
    let engine = script(|_| says("Only once"));
    let page = draw(0.0, 0.0, 612.0, 792.0);
    let bytes = pdf(format!("{page}{page}{page}"));
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
    assert_eq!(doc.markdown.matches("Only once").count(), 1);
}

/// 上面一层认不出字时接着认下一层。
#[test]
fn the_next_layer_is_tried_when_one_is_blank() {
    let engine = script(|n| {
        if n == 0 {
            Ok(OcrOutput::default())
        } else {
            says("Second layer")
        }
    });
    let page = draw(0.0, 0.0, 612.0, 792.0);
    let bytes = pdf(format!("{page}{page}"));
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    assert!(doc.markdown.contains("Second layer"), "{}", doc.markdown);
}

/// 没有图够得上覆盖率时：从大到小试，第一张有结果就停。
#[test]
fn small_images_stop_at_the_first_result() {
    let engine = script(|n| {
        if n == 0 {
            Ok(OcrOutput::default())
        } else {
            says("Found")
        }
    });
    let bytes = pdf(format!(
        "{}{}{}{}",
        draw(0.0, 0.0, 50.0, 50.0),
        draw(100.0, 0.0, 60.0, 60.0),
        draw(200.0, 0.0, 70.0, 70.0),
        body()
    ));
    let doc = extract_with(&bytes, &options(&engine, true)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    assert!(doc.markdown.contains("Found"), "{}", doc.markdown);
}

/// 识别报错不再被吞掉，按文档带回；说明照写。
#[test]
fn errors_come_back_with_the_document() {
    let engine = script(|_| Err("NaN in logits".into()));
    let bytes = pdf(draw(0.0, 0.0, 612.0, 792.0));
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert!(doc.ocr.ran);
    assert_eq!(doc.ocr.error.as_deref(), Some("NaN in logits"));
    assert!(doc.markdown.contains("scanned image"), "{}", doc.markdown);
    assert!(!doc.pages[0].ocr);
}

/// 引擎还没就绪（模型在下载）：不白解图、不调识别，文档照常返回并带原因。
#[test]
fn an_engine_that_is_not_ready_is_not_asked() {
    let engine = std::sync::Arc::new(Script {
        calls: AtomicUsize::new(0),
        deadlines: Mutex::new(Vec::new()),
        not_ready: Some("OCR models are still downloading".into()),
        respond: Box::new(|_| says("no")),
    });
    let bytes = pdf(draw(0.0, 0.0, 612.0, 792.0));
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        doc.ocr.error.as_deref(),
        Some("OCR models are still downloading")
    );
}

/// 引擎 panic 只丢这张图，文档照常返回。
#[test]
fn an_engine_panic_keeps_the_document() {
    let engine = script(|_| panic_any("模型 panic"));
    let bytes = pdf(format!("{}{}", draw(500.0, 720.0, 50.0, 50.0), body()));
    let doc = extract_with(&bytes, &options(&engine, true)).unwrap();
    assert!(doc.markdown.contains("The body text"), "{}", doc.markdown);
    assert!(doc.ocr.error.unwrap().contains("panicked"));
}

/// 文字页不碰引擎，也不算走了 OCR。
#[test]
fn text_pages_do_not_count_as_ocr() {
    let engine = script(|_| says("no"));
    let doc = extract_with(&pdf(body()), &options(&engine, false)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 0);
    assert_eq!(doc.ocr, Default::default());
}

/// 设备和退回按文档报告：上一份文档的退回不粘到下一份上，设备是这份文档实际用的。
#[test]
fn fallback_and_device_belong_to_one_document() {
    let engine = script(|n| {
        let mut out = says("Words")?;
        out.device = Some("cuda:0".into());
        out.fallback = (n == 0).then(|| "out of memory".to_owned());
        Ok(out)
    });
    let bytes = pdf(draw(0.0, 0.0, 612.0, 792.0));
    let first = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(first.ocr.fallback.as_deref(), Some("out of memory"));
    assert_eq!(first.ocr.device.as_deref(), Some("cuda:0"));
    let second = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(second.ocr.fallback, None);
    assert_eq!(second.ocr.device.as_deref(), Some("cuda:0"));
}

/// 把测试图 /Im1 换掉。
fn with_image(content: String, entries: lopdf::Dictionary, data: Vec<u8>) -> Vec<u8> {
    let mut doc = Builder::new(vec![Page::text(content)]).document();
    let id = doc
        .objects
        .iter()
        .find_map(|(id, o)| match o {
            Object::Stream(s) if s.dict.has(b"Width") => Some(*id),
            _ => None,
        })
        .unwrap();
    doc.objects
        .insert(id, Object::Stream(lopdf::Stream::new(entries, data)));
    save_raw(doc)
}

/// 解得开但放不进张量内存上限的图不交给引擎，并说明原因（缺省上限下 16000×16000 就是这样）。
#[test]
fn images_over_the_tensor_budget_are_refused() {
    let engine = script(|_| says("no"));
    let bytes = with_image(
        draw(0.0, 0.0, 612.0, 792.0),
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => 20, "Height" => 20, "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![255; 400],
    );
    let opts = Options {
        // [策略] 解图上限 1 KB，放得下这张图；张量上限 2 KB / 14 约 146 像素，放不下
        max_stream_bytes: 256,
        ..options(&engine, false)
    };
    let doc = extract_with(&bytes, &opts).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 0);
    assert!(doc.ocr.error.unwrap().contains("too large"));
}

/// 5 个分量的 JPEG：不是“parser panicked”，这张图跳过，文档照常返回。
#[test]
fn a_jpeg_with_five_components_is_skipped() {
    let engine = script(|_| says("no"));
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 23, 8, 0, 8, 0, 8, 5];
    for id in 1..=5u8 {
        jpeg.extend_from_slice(&[id, 0x11, 0]);
    }
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    let bytes = with_image(
        draw(0.0, 0.0, 612.0, 792.0),
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => 8, "Height" => 8, "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        },
        jpeg,
    );
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert_eq!(engine.calls.load(Ordering::SeqCst), 0);
    assert!(doc.markdown.contains("scanned image"), "{}", doc.markdown);
}

/// OCR 的时间不算进按字节估的时限：识别比那个时限还慢，文档也能解完。
#[test]
fn ocr_time_does_not_count_against_the_adaptive_timeout() {
    let bytes = pdf(draw(0.0, 0.0, 612.0, 792.0));
    let limit = parse_timeout(bytes.len(), None);
    let engine = script(move |_| {
        std::thread::sleep(limit + Duration::from_millis(100));
        says("Slow words")
    });
    let doc = extract_with(&bytes, &options(&engine, false)).unwrap();
    assert!(doc.markdown.contains("Slow words"));
    // 没有请求时限时引擎拿不到截止时间：只受硬上限约束
    assert_eq!(engine.deadlines.lock().unwrap()[0], None);
    // 含 OCR 的耗时不进回归
    assert_eq!(parse_timeout(bytes.len(), None), limit);
}

/// 用户给的时限是硬上限：交给引擎，过了就是超时。
#[test]
fn the_user_timeout_still_stops_ocr() {
    let bytes = pdf(draw(0.0, 0.0, 612.0, 792.0));
    let engine = script(|_| {
        std::thread::sleep(Duration::from_millis(300));
        says("Too late")
    });
    let opts = Options {
        timeout: Some(Duration::from_millis(100)),
        ..options(&engine, false)
    };
    assert_eq!(extract_with(&bytes, &opts), Err(Error::Timeout));
    assert!(engine.deadlines.lock().unwrap()[0].is_some());
}
