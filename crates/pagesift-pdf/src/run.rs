//! 一份 PDF 的处理流程：打开文件 → 逐页解释、复核、识别图片、认表格和图区 → 去页眉页脚 →
//! 分段 → 定标题 → 渲染 Markdown。

use std::time::{Duration, Instant};

use crate::file::{Document, Object, OpenError};
use crate::obj::deref;
use crate::*;

const SCANNED_MAX_CHARS: usize = 32;
/// [策略] 图盖住页面这么大比例才可能是扫描页（0.3 是原值，只是改成 crate 内可见，OCR 也用它）
pub(crate) const SCANNED_MIN_COVERAGE: f64 = 0.3;

/// `use_structure`：用不用写入方声明的结构（结构树、书签）。关掉只用于评测推断本身的水平。
/// `hard` 是用户给的时限；`opts.timeout` 已经和按字节估的时限取过小，那个估计没算 OCR，要按 OCR 的
/// 耗时后移（但不超过 `hard`）。
pub(crate) fn run(
    bytes: &[u8],
    opts: &Options,
    hard: Option<Duration>,
    use_structure: bool,
) -> Result<PdfDocument, Error> {
    let start = Instant::now();
    let mut deadline = opts.timeout.map(|t| start + t);
    let hard = hard.and_then(|t| start.checked_add(t));
    let _budget = budget::enter(deadline);
    if bytes.len() > opts.max_input_bytes {
        return Err(Error::TooLarge("input size"));
    }
    let head = &bytes[..bytes.len().min(1024)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(Error::Malformed("missing %PDF header".into()));
    }
    let doc = Document::open(bytes, opts.max_stream_bytes).map_err(|e| match e {
        OpenError::Encrypted => Error::Encrypted,
        OpenError::Malformed(m) => Error::Malformed(m.into()),
        OpenError::Timeout => Error::Timeout,
    })?;
    check_deadline(deadline)?;

    let tree = doc.page_tree();
    let mut page_ids = tree.ids.clone();
    let total_pages = page_ids.len();
    if let Some(limit) = opts.page_limit {
        page_ids.truncate(limit.max(1));
    }
    if page_ids.len() > opts.max_pages {
        return Err(Error::TooLarge("page count"));
    }
    if page_ids.is_empty() {
        return Err(Error::Malformed("document has no pages".into()));
    }

    let structure = if use_structure {
        structure::read(&doc, &page_ids)
    } else {
        structure::Structure::default()
    };
    let mut tagged_lines = 0;
    let mut bases = Vec::with_capacity(page_ids.len());

    // 书签条目按页分好组，标题匹配的长度上限也只算一次
    let outline_by_page = structure::outline_by_page(&structure.outline, page_ids.len());
    let outline_limit = structure::title_limit(&structure.outline);
    // 调试输出开关只读一次
    let dump_tables = std::env::var_os("PAGESIFT_PDF_TABLES").is_some();
    let mut fonts = page::FontCaches::default();
    let mut images = image::Images::default();
    let mut referenced = Vec::new();
    let mut page_images = Vec::with_capacity(page_ids.len());
    let mut qualities = Vec::with_capacity(page_ids.len());
    let mut frags = Vec::with_capacity(page_ids.len());
    let mut heights = Vec::with_capacity(page_ids.len());
    let mut stats = Vec::with_capacity(page_ids.len());
    let mut ocr_pages = Vec::with_capacity(page_ids.len());
    let mut ocr_report = ocr::OcrReport::default();
    for (index, &id) in page_ids.iter().enumerate() {
        check_deadline(deadline)?;
        let page = match doc.get_object(id).map(|o| deref(&doc, o)) {
            Some(Object::Dictionary(d)) => d,
            _ => {
                frags.push(Vec::new());
                bases.push(None);
                heights.push(792.0);
                stats.push((0, 0, false));
                page_images.push(Vec::new());
                qualities.push(PageQuality::default());
                continue;
            }
        };
        // 低分页（乱码、字体映射缺失）换路径重做，只重做这一页（quality.rs）
        let (mut ink, quality) =
            page::interpret(&doc, page, &mut fonts, deadline, opts.max_stream_bytes)?;
        qualities.push(quality);
        // 旋转的字：整页横排的转正，少数竖写的拼成行，后面和别的行一起排版
        let turned_lines = orient::orient(&mut ink);
        let text_chars: usize = ink
            .glyphs
            .iter()
            .map(|g| g.text.chars().filter(|c| !c.is_whitespace()).count())
            .sum();
        let coverage = ink.image_area / (ink.width * ink.height);
        let scanned = ink.image_count > 0
            && coverage >= SCANNED_MIN_COVERAGE
            && text_chars < SCANNED_MAX_CHARS;
        stats.push((text_chars, ink.image_count, scanned));
        // 图片识别：画了哪些图、在哪；扫描页总要识别（说明里引用那张图）
        let force_ocr = opts.force_ocr && opts.ocr.is_some();
        let placed = page::wanted_images(&ink, scanned || force_ocr, &opts.images);
        let idx = images.add(
            &doc,
            index + 1,
            ink.height,
            &placed,
            opts.images.export,
            opts.max_stream_bytes,
        );
        page_images.push(idx.iter().flatten().copied().collect::<Vec<_>>());
        // 扫描页 OCR：识别出文字就当作这页的文字层，不再写扫描页说明
        let mut ocred = false;
        let mut ocr_formulas = Vec::new();
        if (scanned || force_ocr)
            && let Some(engine) = &opts.ocr
        {
            let ocr_start = Instant::now();
            let found = ocr::recognize_page(
                &doc,
                &ink.images,
                !opts.images.any(),
                engine.as_ref(),
                opts.max_stream_bytes.saturating_mul(ocr::RASTER_BUDGET),
                ink.width * ink.height,
                hard,
            );
            deadline = ocr::extend_deadline(deadline, ocr_start.elapsed(), hard);
            budget::set(deadline);
            ocred = !found.is_empty();
            // 扫描页的文字层是零星的水印、页码；强制 OCR 时，认出字的图盖住了足够大的一块才认为文字层
            // 不可信（只认出一个 logo 不能丢掉整页正文）
            if ocred && (scanned || found.covers_page) {
                ink.glyphs.clear();
            }
            ink.glyphs.extend(found.glyphs);
            ocr_formulas = found.formulas;
            ocr_report.merge(found.report);
        }
        ocr_pages.push(ocred);
        let note = scanned && !ocred;
        heights.push(ink.height);
        bases.push(ink.base);
        let mut page_frags =
            layout::fragments(&ink.glyphs, structure.tags.as_ref().map(|t| &t[index]));
        page_frags.extend(turned_lines);
        if let Some(tags) = &structure.tags {
            tagged_lines += structure::tag_lines(&mut page_frags, &tags[index]);
        }
        // 表格：格子里的片段换成一个原样输出的 Markdown 表格，放在表格顶部的位置
        // 书签声明的标题行（这一页和前一页的条目）标成 keep：不归图，也不当页眉页脚删
        structure::keep_outline_lines(&mut page_frags, &outline_by_page[index], outline_limit);
        // 矢量图：图内文字（刻度、图例、标注）不进表格检测、不当标题，合成一个原样输出的块
        if std::env::var_os("PAGESIFT_DBG").is_some() {
            let bent: Vec<_> = ink
                .shapes
                .iter()
                .filter(|s| s.visible && (s.slanted + s.curves) > 0)
                .collect();
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for s in &bent {
                x0 = x0.min(s.x0);
                y0 = y0.min(s.y0);
                x1 = x1.max(s.x1);
                y1 = y1.max(s.y1);
            }
            eprintln!(
                "dbg {} w {} h {} shapes {} bent {} box {:.0} {:.0} {:.0} {:.0} rules {}",
                index + 1,
                ink.width,
                ink.height,
                ink.shapes.len(),
                bent.len(),
                x0,
                y0,
                x1,
                y1,
                ink.rules.len()
            );
        }
        let figures = figure::detect(&ink.shapes, &ink.rules, &page_frags, ink.width, ink.height);
        ink.shapes = Vec::new();
        if dump_tables {
            for f in &figures {
                let head: String = figure::text_of(&page_frags, &f.lines)
                    .chars()
                    .take(100)
                    .collect();
                eprintln!(
                    "figure\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{}\t{}",
                    index + 1,
                    f.x0,
                    f.top,
                    f.x1,
                    f.bottom,
                    f.lines.len(),
                    head.replace('\n', " / ")
                );
            }
        }
        let (figure_blocks, table_rules) =
            figure::take(&figures, &mut page_frags, std::mem::take(&mut ink.rules));
        // 没被图区认领的旋转行不参加表格识别，识别完再放回去
        let (turned_left, mut rest): (Vec<_>, Vec<_>) = std::mem::take(&mut page_frags)
            .into_iter()
            .partition(|l| l.turned);
        let tables = table::extract(&table_rules, &mut rest, &figures, ink.width, ink.height);
        page_frags = rest;
        page_frags.extend(turned_left);
        page_frags.extend(figure_blocks);
        if dump_tables {
            for t in &tables {
                let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
                let head: Vec<String> = t.rows.iter().take(3).map(|r| r.join(" | ")).collect();
                eprintln!(
                    "table\t{}\t{:.0}\t{:.0}\t{:.0}\t{:.0}\t{}x{}\t{}",
                    index + 1,
                    t.x0,
                    t.top,
                    t.x1,
                    t.bottom,
                    t.rows.len(),
                    cols,
                    head.join(" // ")
                );
            }
        }
        if !tables.is_empty() {
            let mut sizes: Vec<f64> = page_frags.iter().map(|f| f.size).collect();
            sizes.sort_by(f64::total_cmp);
            let size = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0);
            for t in tables {
                page_frags.push(layout::Line {
                    text: t.markdown(),
                    x0: t.x0,
                    x1: t.x1,
                    y: t.top,
                    size,
                    bold: false,
                    raw: true,
                    mcid: None,
                    artifact: false,
                    hint: None,
                    group: None,
                    keep: false,
                    turned: false,
                });
            }
        }
        // 图片引用和扫描页说明：原样输出的行，放在图的位置
        let mut sizes: Vec<f64> = page_frags.iter().map(|f| f.size).collect();
        sizes.sort_by(f64::total_cmp);
        let size = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0); // [策略] 页面没有字时引用行的字号，同上面表格行的取法
        let mut lines = page::image_lines(
            &images,
            &placed,
            &idx,
            note,
            index + 1,
            opts.images.markdown,
            &mut referenced,
            size,
        );
        lines.extend(ocr_formulas.into_iter().map(|f| layout::Line {
            text: f.text,
            x0: f.x0,
            x1: f.x1,
            y: f.top,
            size,
            bold: false,
            raw: true,
            mcid: None,
            artifact: false,
            hint: None,
            group: None,
            keep: true,
            turned: false,
        }));
        // 扫描页的图读不出来：只写说明，放在页顶
        if note && !lines.iter().any(|l| l.keep) {
            lines.push(layout::Line {
                text: page::scan_note(index + 1),
                x0: 0.0,
                x1: ink.width,
                y: ink.height,
                size,
                bold: false,
                raw: true,
                mcid: None,
                artifact: false,
                hint: None,
                group: None,
                keep: true,
                turned: false,
            });
        }
        page_frags.extend(lines);
        frags.push(page_frags);
    }
    check_deadline(deadline)?;

    // 书签标题行的 keep 已在逐页循环里标过（图区识别之前），这里不再重复
    layout::remove_furniture(&mut frags, &heights);
    let mut paras = Vec::with_capacity(frags.len());
    for f in frags {
        check_deadline(deadline)?;
        paras.push(layout::paragraphs(layout::blocks(f)));
    }
    // 写入方给了标题就照着用：结构树优先，其次书签（对上一半以上的条目才算数）
    // 结构树里只有一两个标题（常见的是只标了文档题名）说明写入方没给章节打标签，不能拿它定标题
    let mode = if tagged_lines >= 2 {
        // 书签里有、结构树没标成标题的（常见：粗体段落当小节标题），也算标题
        structure::mark_outline(&mut paras, &structure.outline, &bases);
        layout::Mode::Hinted
    } else {
        let located = structure
            .outline
            .iter()
            .filter(|e| e.page.is_some())
            .count();
        // 结构树没起作用时，行上的少量标签提示不能留着（否则会把段落切碎）
        for p in paras.iter_mut().flatten() {
            p.hint = None;
        }
        let matched = structure::mark_outline(&mut paras, &structure.outline, &bases);
        if matched >= 2 && matched * 2 >= located {
            layout::Mode::Hinted
        } else {
            layout::Mode::Infer
        }
    };
    let (page_md, markdown) = layout::render(&paras, mode);

    let pages: Vec<PdfPage> = page_md
        .into_iter()
        .zip(stats)
        .zip(qualities.into_iter().zip(page_images))
        .enumerate()
        .map(
            |(i, ((md, (text_chars, image_count, scanned)), (quality, images)))| PdfPage {
                number: i + 1,
                markdown: md,
                text_chars,
                image_count,
                scanned,
                quality,
                images,
                ocr: ocr_pages[i],
            },
        )
        .collect();
    let scanned_pages = pages.iter().filter(|p| p.scanned).count();
    let scanned = scanned_pages > 0 && scanned_pages * 2 >= pages.len();
    let mut metadata = metadata(&doc);
    metadata.page_count = total_pages;
    let inferred_title = pages
        .first()
        .and_then(|p| structure::inferred_title(&p.markdown));
    // 对象都读完了，这时才知道有没有靠扫描纠正过偏移
    let mut damage = doc.damage(&tree);
    // 修复过的文件再查一遍：页面字典在、页数对，内容流或字体却可能丢了（截断时常见）
    if damage.repaired || damage.rescued {
        (damage.incomplete_pages, damage.fontless_pages) =
            doc.incomplete_pages(&page_ids, opts.max_stream_bytes);
    }
    let warnings = damage.warnings(total_pages);
    Ok(PdfDocument {
        markdown,
        pages,
        metadata,
        scanned,
        inferred_title,
        warnings,
        images: images.into_list(),
        ocr: ocr_report,
    })
}
