//! 和 html5ever（经 scraper）逐页对比：文本序列、元素序列是否一致，以及解析速度。
//! `cargo run --release -p pagesift-html --example compare -- bench/data/inputs [show]`

use std::time::Instant;

/// 先序遍历的签名：元素名（带深度）和折叠空白后的文本。
fn sig_ours(html: &str) -> (Vec<String>, Vec<String>) {
    let doc = pagesift_html::Html::parse(html);
    let (mut els, mut texts) = (Vec::new(), Vec::new());
    for n in doc.root().descendants() {
        if let Some(e) = n.as_element() {
            els.push(format!(
                "{}{}",
                n.ancestors().filter(|a| a.is_element()).count(),
                e.name().to_ascii_lowercase()
            ));
        } else if let Some(t) = n.as_text() {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                texts.push(t);
            }
        }
    }
    (els, texts)
}

fn sig_html5ever(html: &str) -> (Vec<String>, Vec<String>) {
    let doc = scraper::Html::parse_document(html);
    let (mut els, mut texts) = (Vec::new(), Vec::new());
    for n in doc.tree.root().descendants() {
        if let Some(e) = n.value().as_element() {
            els.push(format!(
                "{}{}",
                n.ancestors().filter(|a| a.value().is_element()).count(),
                e.name().to_ascii_lowercase()
            ));
        } else if let Some(t) = n.value().as_text() {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                texts.push(t);
            }
        }
    }
    (els, texts)
}

fn first_diff(a: &[String], b: &[String]) -> usize {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: compare <dir> [show]");
    let show: usize = args.next().map_or(0, |v| v.parse().unwrap());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.path()))
        .collect();
    files.sort();
    let pages: Vec<(String, String)> = files
        .iter()
        .filter_map(|p| {
            Some((
                p.display().to_string(),
                String::from_utf8_lossy(&std::fs::read(p).ok()?).into_owned(),
            ))
        })
        .collect();

    let (mut same_text, mut same_els, mut shown) = (0, 0, 0);
    let (mut text_total, mut text_common) = (0usize, 0usize);
    for (path, html) in &pages {
        let (ea, ta) = sig_ours(html);
        let (eb, tb) = sig_html5ever(html);
        same_text += (ta == tb) as usize;
        same_els += (ea == eb) as usize;
        let ja = ta.join(" ");
        let jb = tb.join(" ");
        text_total += jb.len();
        text_common += jb.len().min(ja.len())
            - (jb.len() as isize - ja.len() as isize)
                .unsigned_abs()
                .min(jb.len().min(ja.len()));
        if (ta != tb || ea != eb) && shown < show {
            shown += 1;
            let i = first_diff(&ta, &tb);
            let j = first_diff(&ea, &eb);
            println!(
                "== {path}: texts {}/{} first diff #{i}, elements {}/{} first diff #{j}",
                ta.len(),
                tb.len(),
                ea.len(),
                eb.len()
            );
            println!(
                "   ours  text: {:?}",
                ta.get(i).map(|s| &s[..s.len().min(120)])
            );
            println!(
                "   h5e   text: {:?}",
                tb.get(i).map(|s| &s[..s.len().min(120)])
            );
            println!(
                "   ours  els : {:?}",
                &ea[j.saturating_sub(3)..(j + 4).min(ea.len())]
            );
            println!(
                "   h5e   els : {:?}",
                &eb[j.saturating_sub(3)..(j + 4).min(eb.len())]
            );
        }
    }
    let n = pages.len();
    println!(
        "{n} pages: identical text {same_text} ({:.1}%), identical element tree {same_els} ({:.1}%), text length ratio {:.4}",
        same_text as f64 * 100.0 / n as f64,
        same_els as f64 * 100.0 / n as f64,
        text_common as f64 / text_total as f64
    );

    let bytes: usize = pages.iter().map(|p| p.1.len()).sum();
    for (name, f) in [
        (
            "pagesift-html",
            &(|h: &str| pagesift_html::Html::parse(h).len()) as &dyn Fn(&str) -> usize,
        ),
        ("html5ever", &|h: &str| {
            scraper::Html::parse_document(h).tree.nodes().count()
        }),
    ] {
        let t = Instant::now();
        let mut sink = 0;
        for _ in 0..3 {
            for (_, h) in &pages {
                sink += f(h);
            }
        }
        let s = t.elapsed().as_secs_f64();
        println!(
            "{name:>14}: {:7.1} MB/s, {:.3} ms/page ({sink} nodes)",
            (3 * bytes) as f64 / 1e6 / s,
            s * 1000.0 / (3 * n) as f64
        );
    }
}
