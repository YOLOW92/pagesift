mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use common::{Builder, Page, paragraph};
use lopdf::{Document, Object, Stream, dictionary};
use pagesift_pdf::extract;

#[test]
fn adversarial_form_xobject_recursion_bomb() {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    // 构建循环递归的 Form XObject: FormA 调用 FormB, FormB 调用 FormA
    let form_a_id = doc.new_object_id();
    let form_b_id = doc.new_object_id();

    let stream_a = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "FmB" => form_b_id },
            },
        },
        b"/FmB Do\n".to_vec(),
    );
    let stream_b = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "FmA" => form_a_id },
            },
        },
        b"/FmA Do\n".to_vec(),
    );

    doc.objects.insert(form_a_id, Object::Stream(stream_a));
    doc.objects.insert(form_b_id, Object::Stream(stream_b));

    let content_stream = Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 100 100 Td (Root Text) Tj ET /FmA Do\n".to_vec(),
    );
    let cid = doc.add_object(content_stream);
    let f1 = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1",
        "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
    });

    let resources = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => f1 },
        "XObject" => dictionary! { "FmA" => form_a_id },
    });

    let pid = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => cid,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![pid.into()], "Count" => 1,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();

    let t0 = Instant::now();
    let res = extract(&bytes);
    let elapsed = t0.elapsed();

    // 严防栈溢出：必须在短时间内（< 100ms）正常返回且截断，不能 Panic 崩溃
    assert!(res.is_ok());
    assert!(elapsed.as_millis() < 100);
    let text = res.unwrap().markdown;
    assert!(text.contains("Root Text"));
}

#[test]
fn pathological_micro_rules_storm() {
    let mut content = String::from("BT /F1 12 Tf 72 700 Td (Testing Rule Flood) Tj ET\n");
    // 注入 15,000 条细微杂乱且相交的微型线段
    for i in 0..15_000 {
        let x = (i % 500) as f64;
        let y = (i % 700) as f64;
        content.push_str(&format!("{x} {y} m {} {} l S\n", x + 5.0, y));
    }
    let doc = Builder::new(vec![Page::text(content)]).build();
    let t0 = Instant::now();
    let res = extract(&doc);
    let dt = t0.elapsed();
    assert!(res.is_ok());
    // 规则上限截断保护（MAX_RULES_PER_PAGE），确保在毫秒级内完成，无二次方爆炸
    assert!(dt.as_millis() < 250);
}

#[test]
fn adversarial_font_matrices_and_corrupt_floats() {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    // 注入零行列式、超大溢出数与负缩放
    let f_zero = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "FontMatrix" => vec![0.into(), 0.into(), 0.into(), 0.into(), 0.into(), 0.into()],
        "FirstChar" => 0, "LastChar" => 1,
        "Widths" => vec![500.into()],
        "CharProcs" => dictionary! {},
        "Encoding" => "StandardEncoding",
    });
    let f_huge = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type3",
        "FontBBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "FontMatrix" => vec![1e25.into(), 0.into(), 0.into(), 1e25.into(), 0.into(), 0.into()],
        "FirstChar" => 0, "LastChar" => 1,
        "Widths" => vec![500.into()],
        "CharProcs" => dictionary! {},
        "Encoding" => "StandardEncoding",
    });

    let cid = doc.add_object(Stream::new(
        dictionary! {},
        b"BT /FZ 1 Tf 100 100 Td (Zero) Tj /FH 1 Tf 100 150 Td (Huge) Tj ET\n".to_vec(),
    ));
    let resources = doc.add_object(dictionary! {
        "Font" => dictionary! { "FZ" => f_zero, "FH" => f_huge },
    });
    let pid = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => cid,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![pid.into()], "Count" => 1,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();

    let res = extract(&bytes);
    assert!(res.is_ok());
}

#[test]
fn massive_500_page_continuous_document_stress() {
    let mut pages = Vec::with_capacity(500);
    for p in 1..=500 {
        let mut content =
            format!("BT /F2 16 Tf 72 720 Td (Chapter {p}: In-Depth System Verification) Tj ET\n");
        let (p1, ny) = paragraph(
            11.0,
            72.0,
            690.0,
            14.0,
            80,
            &format!(
                "This is page number {p} of the massive 500-page continuous document stress suite. Testing heap allocation, text clustering, and stability."
            ),
        );
        content.push_str(&p1);
        let (p2, _) = paragraph(
            11.0,
            72.0,
            ny - 10.0,
            14.0,
            80,
            "Advanced agentic coding requires zero memory leaks, deterministic memory reclamation, and consistent latency across thousands of paragraphs.",
        );
        content.push_str(&p2);
        // 每 10 页插入一个完整的带框线表格
        if p % 10 == 0 {
            content.push_str("72 500 468 100 re S\n");
            content.push_str("72 550 468 0 re S\n");
            content.push_str("200 500 0 100 re S\n");
            content.push_str("BT /F1 10 Tf 80 560 Td (Key Metric) Tj 220 560 Td (Score) Tj ET\n");
            content.push_str(
                "BT /F1 10 Tf 80 520 Td (Throughput) Tj 220 520 Td (100000 req/s) Tj ET\n",
            );
        }
        pages.push(Page::text(content));
    }

    let doc_bytes = Builder::new(pages).build();
    eprintln!(
        "Built 500-page synthetic PDF: {:.2} MB",
        doc_bytes.len() as f64 / 1_048_576.0
    );

    let t0 = Instant::now();
    let res = extract(&doc_bytes);
    let dt = t0.elapsed();

    assert!(res.is_ok());
    let doc = res.unwrap();
    assert_eq!(doc.pages.len(), 500);
    assert!(doc.markdown.contains("This is page number 1"));
    assert!(doc.markdown.contains("This is page number 500"));
    eprintln!(
        "Processed 500 pages in {:.2}s ({:.2} ms/page)",
        dt.as_secs_f64(),
        dt.as_secs_f64() * 1000.0 / 500.0
    );
    assert!(dt.as_secs_f64() < 5.0, "500 pages must parse under 5s");
}

fn walk_pdfs(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_pdfs(&p, out);
            } else if p.extension().is_some_and(|ext| ext == "pdf") {
                out.push(p);
            }
        }
    }
}

#[test]
fn high_concurrency_saturation_stress_olmocr() {
    let mut pdf_dir = PathBuf::from("bench/pdf/out/pdfs");
    if !pdf_dir.exists() {
        pdf_dir = PathBuf::from("../../bench/pdf/out/pdfs");
    }
    if !pdf_dir.exists() {
        eprintln!("olmocr-bench pdf directory not found, skipping saturation test.");
        return;
    }
    let mut files = Vec::new();
    walk_pdfs(&pdf_dir, &mut files);
    if files.is_empty() {
        return;
    }

    let num_threads = std::thread::available_parallelism().map_or(8, |n| n.get());
    let chunk_size = files.len().div_ceil(num_threads);
    let chunks: Vec<Vec<PathBuf>> = files.chunks(chunk_size).map(|c| c.to_vec()).collect();

    let total_parsed = Arc::new(AtomicUsize::new(0));
    let total_bytes = Arc::new(AtomicUsize::new(0));

    let t0 = Instant::now();
    std::thread::scope(|s| {
        for chunk in chunks {
            let parsed_counter = Arc::clone(&total_parsed);
            let bytes_counter = Arc::clone(&total_bytes);
            s.spawn(move || {
                for path in chunk {
                    if let Ok(data) = fs::read(&path) {
                        bytes_counter.fetch_add(data.len(), Ordering::Relaxed);
                        if let Ok(doc) = extract(&data) {
                            parsed_counter.fetch_add(doc.pages.len(), Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });

    let elapsed = t0.elapsed();
    let total_p = total_parsed.load(Ordering::SeqCst);
    let total_b = total_bytes.load(Ordering::SeqCst);
    let rate = total_p as f64 / elapsed.as_secs_f64();
    let mb = total_b as f64 / (1024.0 * 1024.0);

    eprintln!(
        "Concurrent stress ({num_threads} threads): {total_p} pages ({mb:.1} MB) in {:.2}s => {:.1} pages/sec",
        elapsed.as_secs_f64(),
        rate
    );
    assert!(total_p >= 1200);
    let min_rate = if cfg!(debug_assertions) { 100.0 } else { 800.0 };
    assert!(
        rate > min_rate,
        "Parallel saturation rate must exceed {min_rate} pages/sec"
    );
}
