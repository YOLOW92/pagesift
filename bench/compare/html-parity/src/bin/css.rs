//! CSS 选择器对拍：同一批选择器在 scraper 和 pagesift-html 上命中的元素（名字 + 文本 + outer HTML）是否一致。
//! 在 bench/compare/html-parity 下运行：
//!   cargo run --release --bin css -- parity ../../data/inputs [show] [templates]
//!   cargo run --release --bin css -- profile ../../data/inputs       每个选择器两边的耗时
//!   perf stat -e instructions target/release/css bench <old|new> <parse|select> ../../data/inputs
//! bench 模式给 `perf stat -e instructions` 用：select 减去 parse 就是选择器的开销。

use std::collections::HashMap;
use std::hint::black_box;

const FIXED: &[&str] = &[
    "a",
    "p",
    "div",
    "*",
    "span",
    "a[href]",
    "a[href^='http']",
    "a[href^=https]",
    "a[href$='.pdf' i]",
    "a[href*='?']",
    "a[href*=\"utm\" i]",
    "a:not([href^='http'])",
    "a[rel~=nofollow]",
    "a[target=_BLANK]",
    "img[alt]",
    "img[src$='.png']",
    "[id]",
    "[lang|=en]",
    "[data-testid]",
    "[aria-label]",
    "[class*=nav]",
    "[class^=col-]",
    "[role=navigation] a",
    "meta[name][content]",
    "meta[property^='og:']",
    "link[rel=stylesheet]",
    "input[type=TEXT]",
    "script[src]",
    "div > p",
    "div p",
    "body > *",
    "body > div > div",
    "h1 + p",
    "h2 ~ p",
    "h2 + *",
    "li + li",
    "li ~ li > a",
    "header nav ul li a",
    "article h2 a",
    "table tr td",
    "table tr td:nth-child(1)",
    "tr:nth-child(even) > td",
    "ul > li:first-child",
    "li:last-child",
    "li:only-child",
    "li:nth-child(2n+1)",
    "li:nth-child(-n+3)",
    "li:nth-child(3n - 1)",
    "li:nth-last-child(2)",
    "p:nth-of-type(2)",
    "p:nth-last-of-type(1)",
    "section:first-of-type",
    "td:last-of-type",
    "span:only-child",
    "*:only-of-type",
    "div:empty",
    "p:empty",
    ":root",
    ":root > body",
    ":scope > head > title",
    ":is(h1, h2, h3)",
    ":is(h1, h2, h3) > a",
    ":where(nav, footer) a",
    "main :is(p, li):not(:empty)",
    "div:not(.a, .b, [id])",
    "div:has(> img)",
    "li:has(a)",
    "h2:has(+ p)",
    "li:has(~ li)",
    "svg",
    "svg path",
    "svg *",
    "noscript",
    "template *",
    "form input:not([type=hidden])",
    "select > option:first-child",
    "dl > dt + dd",
    "figure > figcaption",
    "pre > code",
];

/// 页面里最常见的 class 和 id 生成的选择器，覆盖站点特有的写法。
fn derived(html: &str) -> Vec<String> {
    let doc = pagesift_html::Html::parse(html);
    let mut classes: HashMap<&str, usize> = HashMap::new();
    let mut ids = Vec::new();
    for n in doc.root().descendants() {
        let Some(e) = n.as_element() else { continue };
        if let Some(c) = e.attr("class") {
            for c in c.split_ascii_whitespace() {
                *classes.entry(c).or_default() += 1;
            }
        }
        if let Some(id) = e.attr("id")
            && ids.len() < 5
            && !id.is_empty()
        {
            ids.push(id);
        }
    }
    let mut classes: Vec<_> = classes.into_iter().collect();
    classes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut out = Vec::new();
    for (c, _) in classes.iter().take(8) {
        let c = escape(c);
        out.push(format!(".{c}"));
        out.push(format!("div.{c} > *"));
        out.push(format!(".{c} a[href]"));
        out.push(format!(".{c} + .{c}"));
        out.push(format!(".{c}:nth-child(2n)"));
        out.push(format!(
            "[class~='{}']",
            c.replace('\\', "\\\\").replace('\'', "\\'")
        ));
    }
    if let [(a, _), (b, _), ..] = classes.as_slice() {
        out.push(format!(".{} .{}", escape(a), escape(b)));
        out.push(format!(".{}, .{}", escape(a), escape(b)));
    }
    for id in ids {
        let id = escape(id);
        out.push(format!("#{id}"));
        out.push(format!("#{id} *"));
        out.push(format!(
            "[id='{}'] > *",
            id.replace('\\', "\\\\").replace('\'', "\\'")
        ));
    }
    out
}

/// CSS 标识符转义：非 [A-Za-z0-9_-] 和非 ASCII 以外的字符加反斜杠，开头的数字写成十六进制转义。
fn escape(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        let digit_start = c.is_ascii_digit() && (i == 0 || (i == 1 && s.starts_with('-')));
        if digit_start {
            out.push_str(&format!("\\{:x} ", c as u32));
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '-' || !c.is_ascii() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    if out == "-" {
        out = "\\-".into();
    }
    out
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 命中元素的签名：名字、文本、outer HTML（只比前几个，太长的截断）。
fn sig(name: &str, text: &str, html: Option<String>) -> String {
    let mut s = format!("<{name}> {}", text);
    if let Some(h) = html {
        s.push_str(" || ");
        s.push_str(&sort_attrs(&h));
    }
    // html5ever 会把 SVG 的元素名、属性名改回驼峰（linearGradient、viewBox），pagesift-html 保持小写；
    // 两个解析器对 head 里空白文本节点的取舍不同，空白一律去掉
    s.split_whitespace()
        .collect::<String>()
        .to_lowercase()
        .chars()
        .take(4000)
        .collect()
}

/// scraper 序列化时属性按名字排序，pagesift-html 保留源码顺序。比较前把每个开始标签的属性排好序。
/// 注释原样保留；看起来不像序列化器输出的开始标签（脚本里的字符串等）也原样保留。
fn sort_attrs(h: &str) -> String {
    let mut out = String::new();
    let mut rest = h;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map_or(rest.len(), |k| k + 3);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let tag_end = rest[1..].find([' ', '>']).map_or(rest.len(), |j| j + 1);
        if !rest[1..].starts_with(|c: char| c.is_ascii_alphabetic())
            || !rest[tag_end..].starts_with(' ')
        {
            out.push('<');
            rest = &rest[1..];
            continue;
        }
        let mut attrs = Vec::new();
        let mut r = &rest[tag_end..];
        while let Some(t) = r.strip_prefix(' ') {
            let Some(eq) = t.find("=\"") else { break };
            let Some(q) = t[eq + 2..].find('"') else {
                break;
            };
            attrs.push(&t[..eq + 2 + q + 1]);
            r = &t[eq + 2 + q + 1..];
        }
        if !r.starts_with('>') {
            out.push('<');
            rest = &rest[1..];
            continue;
        }
        out.push_str(&rest[..tag_end]);
        attrs.sort();
        for a in attrs {
            out.push(' ');
            out.push_str(a);
        }
        rest = r;
    }
    out.push_str(rest);
    out
}

fn hits_scraper(doc: &scraper::Html, sel: &str) -> Option<Vec<String>> {
    let sel = scraper::Selector::parse(sel).ok()?;
    let root = doc.root_element();
    let mut out = Vec::new();
    for n in doc.tree.root().descendants() {
        let Some(e) = scraper::ElementRef::wrap(n) else {
            continue;
        };
        if sel.matches_with_scope(&e, Some(root)) {
            let html = (out.len() < 20).then(|| e.html());
            out.push(sig(e.value().name(), &e.text().collect::<String>(), html));
        }
    }
    Some(out)
}

fn hits_ours(doc: &pagesift_html::Html<'_>, sel: &str) -> Option<Vec<String>> {
    let sel = pagesift_html::Selector::parse(sel).ok()?;
    let mut out = Vec::new();
    for n in doc.select(&sel) {
        let html = (out.len() < 20).then(|| n.html());
        out.push(sig(n.as_element()?.name(), &n.text(), html));
    }
    Some(out)
}

/// 两边的树是否一致（元素序列和文本序列），不一致时选择器结果的差异可能来自解析器。
fn same_tree(a: &scraper::Html, b: &pagesift_html::Html<'_>) -> bool {
    let sa: Vec<String> = a
        .tree
        .root()
        .descendants()
        .filter_map(|n| {
            n.value()
                .as_element()
                .map(|e| e.name().to_ascii_lowercase())
                .or_else(|| n.value().as_text().map(|t| norm(t)))
        })
        .filter(|s| !s.is_empty())
        .collect();
    let sb: Vec<String> = b
        .root()
        .descendants()
        .filter_map(|n| {
            n.as_element()
                .map(|e| e.name().to_owned())
                .or_else(|| n.as_text().map(norm))
        })
        .filter(|s| !s.is_empty())
        .collect();
    sa == sb
}

fn pages(dir: &str) -> Vec<(String, String)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("input dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let bytes = std::fs::read(&p).ok()?;
            Some((
                p.file_name()?.to_string_lossy().into_owned(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        })
        .collect()
}

/// 去掉 `<template>` 元素。html5ever 把模板内容放进独立的文档片段，选择器不能跨过 template 匹配；
/// pagesift-html 把它们当普通子节点（和 BeautifulSoup 一致）。这个差异不属于选择器引擎，对拍时排除。
fn strip_templates(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(j) = lower[i..].find("<template") {
        out.push_str(&html[i..i + j]);
        match lower[i + j..].find("</template>") {
            Some(k) => i = i + j + k + "</template>".len(),
            None => {
                i = html.len();
                break;
            }
        }
    }
    out.push_str(&html[i..]);
    out
}

/// `x` 里和 `other` 第一个不同的字符附近的一段。
fn around(x: &str, other: Option<&String>) -> String {
    let other = other.map_or("", |o| o.as_str());
    let i = x
        .char_indices()
        .zip(other.chars())
        .find(|((_, a), b)| a != b)
        .map_or(x.len().min(other.len()), |((i, _), _)| i);
    let start = x[..i].char_indices().rev().nth(80).map_or(0, |(j, _)| j);
    x[start..].chars().take(240).collect()
}

fn parity(dir: &str, show: bool, keep_templates: bool) {
    let (mut checks, mut same, mut diff_tree_diff, mut real_diff, mut both_err, mut err_mismatch) =
        (0, 0, 0, 0, 0, 0);
    let mut tree_diff_pages = 0;
    for (name, html) in pages(dir) {
        let html = if keep_templates {
            html
        } else {
            strip_templates(&html)
        };
        let old = scraper::Html::parse_document(&html);
        let new = pagesift_html::Html::parse(&html);
        let trees_match = same_tree(&old, &new);
        if !trees_match {
            tree_diff_pages += 1;
        }
        let sels = FIXED
            .iter()
            .map(|s| s.to_string())
            .chain(derived(&html))
            .collect::<Vec<_>>();
        for s in &sels {
            checks += 1;
            match (hits_scraper(&old, s), hits_ours(&new, s)) {
                (Some(a), Some(b)) if a == b => same += 1,
                (Some(a), Some(b)) => {
                    if trees_match {
                        real_diff += 1;
                    } else {
                        diff_tree_diff += 1;
                    }
                    if show && (trees_match || show) {
                        let i = a
                            .iter()
                            .zip(&b)
                            .position(|(x, y)| x != y)
                            .unwrap_or(a.len().min(b.len()));
                        println!(
                            "DIFF {name} tree_match={trees_match} {s:?}: scraper {} vs ours {} hits; first diff #{i}\n  scraper: {:?}\n  ours:    {:?}",
                            a.len(),
                            b.len(),
                            a.get(i).map(|x| around(x, b.get(i))),
                            b.get(i).map(|x| around(x, a.get(i))),
                        );
                    }
                }
                (None, None) => both_err += 1,
                (a, b) => {
                    err_mismatch += 1;
                    println!(
                        "PARSE {name} {s:?}: scraper ok={} ours ok={}",
                        a.is_some(),
                        b.is_some()
                    );
                }
            }
        }
    }
    println!(
        "checks {checks}: identical {same}, differ on pages with identical trees {real_diff}, \
         differ on pages whose trees differ {diff_tree_diff} ({tree_diff_pages} such pages), \
         both rejected {both_err}, accept/reject mismatch {err_mismatch}"
    );
}

/// 每页把固定选择器都跑一遍，统计命中数；`parse` 只解析，用来扣掉解析的指令数。
fn bench(engine: &str, what: &str, dir: &str) {
    let pages = pages(dir);
    let mut total = 0usize;
    match engine {
        "old" => {
            let sels: Vec<_> = FIXED
                .iter()
                .map(|s| scraper::Selector::parse(s).expect("scraper parses fixed selectors"))
                .collect();
            for (_, html) in &pages {
                let doc = scraper::Html::parse_document(html);
                if what == "select" {
                    for s in &sels {
                        total += doc.root_element().select(s).count();
                    }
                }
                black_box(&doc);
            }
        }
        "new" => {
            let sels: Vec<_> = FIXED
                .iter()
                .map(|s| pagesift_html::Selector::parse(s).expect("we parse fixed selectors"))
                .collect();
            for (_, html) in &pages {
                let doc = pagesift_html::Html::parse(html);
                if what == "select" {
                    for s in &sels {
                        total += doc.root().select(s).count();
                    }
                }
                black_box(&doc);
            }
        }
        _ => panic!("engine must be old or new"),
    }
    println!("{engine} {what}: {} pages, {total} hits", pages.len());
}

/// 每个选择器在两边的耗时（毫秒，越低越好），找慢的选择器用。
fn profile(dir: &str) {
    let pages = pages(dir);
    let old: Vec<_> = pages
        .iter()
        .map(|(_, h)| scraper::Html::parse_document(h))
        .collect();
    let new: Vec<_> = pages
        .iter()
        .map(|(_, h)| pagesift_html::Html::parse(h))
        .collect();
    for s in FIXED {
        let so = scraper::Selector::parse(s).expect("scraper");
        let sn = pagesift_html::Selector::parse(s).expect("ours");
        let t = std::time::Instant::now();
        let a: usize = old
            .iter()
            .map(|d| d.root_element().select(&so).count())
            .sum();
        let to = t.elapsed();
        let t = std::time::Instant::now();
        let b: usize = new.iter().map(|d| d.root().select(&sn).count()).sum();
        let tn = t.elapsed();
        println!(
            "{:>8.1} {:>8.1} {:>5.2}x {a:>7} {b:>7} {s}",
            to.as_secs_f64() * 1e3,
            tn.as_secs_f64() * 1e3,
            to.as_secs_f64() / tn.as_secs_f64()
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("parity") => parity(
            &args[1],
            args.iter().any(|a| a == "show"),
            args.iter().any(|a| a == "templates"),
        ),
        Some("bench") => bench(&args[1], &args[2], &args[3]),
        Some("profile") => profile(&args[1]),
        _ => eprintln!(
            "usage: css_parity parity <dir> [show] [templates] | bench <old|new> <parse|select> <dir>"
        ),
    }
}
