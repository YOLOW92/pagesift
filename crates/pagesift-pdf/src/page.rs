//! 一页的解释与复核、图片识别，以及图片在 Markdown 里的引用行。

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use pagesift_stat::Delta;

use crate::file::{Dictionary, Document, ObjectId};
use crate::font::Font;
use crate::geom::{PageInk, Placed};
use crate::image::Images;
use crate::interp::Interp;
use crate::layout::Line;
use crate::quality::{PageQuality, Recheck, needs_recheck};
use crate::{Error, ImageOptions};

/// 两份字体缓存：常规路径和复核路径（复核路径的字体读法不同，不能共用）。
#[derive(Default)]
pub(crate) struct FontCaches {
    pub(crate) normal: HashMap<ObjectId, Rc<Font>>,
    pub(crate) alt: HashMap<ObjectId, Rc<Font>>,
}

/// 解释一页；按质量分判为低分页时换路径重做这一页，好字多的那次胜出（quality.rs）。
pub(crate) fn interpret(
    doc: &Document,
    page: &Dictionary,
    fonts: &mut FontCaches,
    deadline: Option<Instant>,
    limit: usize,
) -> Result<(PageInk, PageQuality), Error> {
    let ink = Interp::new(doc, &mut fonts.normal, deadline, limit)
        .run_page(page)
        .map_err(|_| Error::Timeout)?;
    let mut quality = PageQuality::from_evidence(&ink.evidence);
    let Some(why) = needs_recheck(&ink.evidence, Delta::DEFAULT) else {
        return Ok((ink, quality));
    };
    quality.rechecked = true;
    let alt = Interp::new(doc, &mut fonts.alt, deadline, limit)
        .alternate()
        .run_page(page)
        .map_err(|_| Error::Timeout)?;
    let mut redone = PageQuality::from_evidence(&alt.evidence);
    let better = match why {
        Recheck::Defects => redone.good() > quality.good(),
        Recheck::OnlyButtons => redone.chars > 0,
    };
    if better {
        redone.rechecked = true;
        redone.replaced = true;
        Ok((alt, redone))
    } else {
        Ok((ink, quality))
    }
}

/// 页面上要识别的图：打开了图片识别就是全部；否则只有扫描页上最大的那张（扫描页的说明要引用它）。
pub(crate) fn wanted_images(ink: &PageInk, scanned: bool, opts: &ImageOptions) -> Vec<Placed> {
    if opts.any() {
        return ink.images.clone();
    }
    if !scanned {
        return Vec::new();
    }
    let area = |p: &Placed| (p.x1 - p.x0) * (p.y1 - p.y0);
    ink.images
        .iter()
        .max_by(|a, b| area(a).total_cmp(&area(b)))
        .cloned()
        .into_iter()
        .collect()
}

/// [策略] 扫描页的说明文字（英文，和其他输出一致）；页码从 1 开始。
pub(crate) fn scan_note(page: usize) -> String {
    format!("*Page {page} is a scanned image with no text layer.*")
}

/// 图片引用和扫描页说明，做成原样输出的行，放到图在页面上的位置（阅读顺序随版面）。
/// `idx` 与 `placed` 一一对应；`referenced` 记下全文已经引用过的图（同一张图只引用一次）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn image_lines(
    images: &Images,
    placed: &[Placed],
    idx: &[Option<usize>],
    scanned: bool,
    page: usize,
    markdown: bool,
    referenced: &mut Vec<bool>,
    size: f64,
) -> Vec<Line> {
    let mut out = Vec::new();
    let line = |text: String, p: &Placed, keep: bool| Line {
        text,
        x0: p.x0,
        x1: p.x1,
        y: p.y1,
        size,
        bold: false,
        raw: true,
        mcid: None,
        artifact: false,
        hint: None,
        group: None,
        keep,
        turned: false,
    };
    // 扫描页：最大的那张图前面加一句说明
    let area = |p: &Placed| (p.x1 - p.x0) * (p.y1 - p.y0);
    let scan = scanned
        .then(|| {
            (0..placed.len())
                .filter(|&i| idx[i].is_some())
                .max_by(|&a, &b| area(&placed[a]).total_cmp(&area(&placed[b])))
        })
        .flatten();
    for (i, p) in placed.iter().enumerate() {
        let Some(k) = idx[i] else { continue };
        if referenced.len() <= k {
            referenced.resize(k + 1, false);
        }
        let is_scan = scan == Some(i);
        if !(is_scan || markdown) || (referenced[k] && !is_scan) {
            continue;
        }
        referenced[k] = true;
        let link = format!("![](images/{})", images.get(k).file_name());
        let text = if is_scan {
            format!("{}\n\n{link}", scan_note(page))
        } else {
            link
        };
        out.push(line(text, p, is_scan));
    }
    out
}
