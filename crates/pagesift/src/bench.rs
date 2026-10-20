//! `pagesift bench`：在本地快照上评测正文提取质量和速度。
//!
//! 数据集是 Firecrawl 的 scrape-content-dataset-v1（`truth_text` 是正文片段，
//! `lie_text` 是导航、页脚这类噪声片段），快照由 `bench/fetch_snapshots.py` 抓取。
//! 两个质量口径：
//! - crw 口径：`truth_text` 按行切开、长度超过 20 字符的行算短语，命中 ≥ 30% 算"找到"；
//! - 严格口径：正文召回率（命中短语比例的平均）和噪声泄漏率（`lie_text` 短语漏进输出的比例的平均），
//!   两者合成一个综合分。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use encoding_rs::Encoding;
use pagesift_content::{Options, process};
use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

const TRUTH_MIN_CHARS: usize = 20;
const LIE_MIN_CHARS: usize = 8;
const FOUND_THRESHOLD: f64 = 0.3;

#[derive(Deserialize)]
struct Row {
    id: String,
    url: String,
    truth_text: String,
    lie_text: String,
}

#[derive(Deserialize)]
struct Meta {
    id: String,
    final_url: Option<String>,
    content_type: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct PageResult<'a> {
    id: &'a str,
    url: &'a str,
    snapshot: bool,
    error: Option<&'a str>,
    truth_total: usize,
    truth_hit: usize,
    truth_recall: f64,
    found: bool,
    lie_total: usize,
    lie_hit: usize,
    lie_leak: f64,
    markdown_chars: usize,
    process_ms: f64,
}

pub struct Args {
    pub dir: PathBuf,
    pub out: Option<PathBuf>,
    pub runs: usize,
    pub limit: Option<usize>,
}

fn phrases(text: &str, min_chars: usize) -> Vec<String> {
    text.split('\n')
        .map(str::trim)
        .filter(|l| l.chars().count() > min_chars)
        .map(str::to_lowercase)
        .collect()
}

pub fn run(args: Args) -> Result<()> {
    let rows = read_rows(&args.dir.join("1-0-0.csv"))?;
    let metas = read_metas(&args.dir.join("snapshots/meta.jsonl"))?;
    let link_re = Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("valid regex");
    let mut out = match &args.out {
        Some(p) => Some(BufWriter::new(
            File::create(p).with_context(|| format!("creating {}", p.display()))?,
        )),
        None => None,
    };

    let mut labeled = 0usize;
    let mut fetched = 0usize;
    let mut found = 0usize;
    let mut found_fetched = 0usize;
    let mut recall_sum = 0.0;
    let mut leak_sum = 0.0;
    let mut leak_pages = 0usize;
    let mut times: Vec<f64> = Vec::new();
    let mut bytes_total = 0usize;
    let mut pdfs = 0usize;

    for row in rows.iter().take(args.limit.unwrap_or(usize::MAX)) {
        let truth = phrases(&row.truth_text, TRUTH_MIN_CHARS);
        if truth.is_empty() {
            continue;
        }
        labeled += 1;
        let lies = phrases(&row.lie_text, LIE_MIN_CHARS);
        let meta = metas.get(&row.id);
        let snapshot = args.dir.join(format!("snapshots/{}.html", row.id));
        let error = meta.and_then(|m| m.error.as_deref());

        let mut result = PageResult {
            id: &row.id,
            url: &row.url,
            snapshot: false,
            error,
            truth_total: truth.len(),
            truth_hit: 0,
            truth_recall: 0.0,
            found: false,
            lie_total: lies.len(),
            lie_hit: 0,
            lie_leak: 0.0,
            markdown_chars: 0,
            process_ms: 0.0,
        };

        if error.is_none()
            && let Ok(bytes) = std::fs::read(&snapshot)
        {
            if is_pdf(&bytes, meta.and_then(|m| m.content_type.as_deref())) {
                // PDF 走 pagesift-pdf，不算进 HTML 提取的评测
                pdfs += 1;
                result.error = Some("pdf");
                if let Some(w) = out.as_mut() {
                    serde_json::to_writer(&mut *w, &result)?;
                    w.write_all(b"\n")?;
                }
                continue;
            }
            fetched += 1;
            result.snapshot = true;
            bytes_total += bytes.len();
            let html = decode(&bytes, meta.and_then(|m| m.content_type.as_deref()));
            let opts = Options {
                base_url: meta
                    .and_then(|m| m.final_url.as_deref())
                    .and_then(|u| Url::parse(u).ok())
                    .or_else(|| Url::parse(&row.url).ok()),
                only_main_content: true,
            };
            let mut best = f64::MAX;
            let mut markdown = String::new();
            for _ in 0..args.runs.max(1) {
                let t = Instant::now();
                let doc = process(&html, &opts);
                best = best.min(t.elapsed().as_secs_f64() * 1000.0);
                markdown = doc.markdown;
            }
            times.push(best);
            result.process_ms = (best * 1000.0).round() / 1000.0;
            result.markdown_chars = markdown.chars().count();

            let hay =
                format!("{markdown}\n{}", link_re.replace_all(&markdown, "$1")).to_lowercase();
            result.truth_hit = truth.iter().filter(|p| hay.contains(p.as_str())).count();
            result.truth_recall = result.truth_hit as f64 / truth.len() as f64;
            result.found = result.truth_recall >= FOUND_THRESHOLD;
            if !lies.is_empty() {
                result.lie_hit = lies.iter().filter(|p| hay.contains(p.as_str())).count();
                result.lie_leak = result.lie_hit as f64 / lies.len() as f64;
                leak_sum += result.lie_leak;
                leak_pages += 1;
            }
            recall_sum += result.truth_recall;
            if result.found {
                found += 1;
                found_fetched += 1;
            }
        }
        if let Some(w) = out.as_mut() {
            serde_json::to_writer(&mut *w, &result)?;
            w.write_all(b"\n")?;
        }
    }
    if let Some(mut w) = out {
        w.flush()?;
    }

    let pct = |a: usize, b: usize| {
        if b == 0 {
            0.0
        } else {
            a as f64 * 100.0 / b as f64
        }
    };
    let recall = if fetched == 0 {
        0.0
    } else {
        recall_sum / fetched as f64
    };
    let leak = if leak_pages == 0 {
        0.0
    } else {
        leak_sum / leak_pages as f64
    };
    let clean = 1.0 - leak;
    let score = if recall + clean == 0.0 {
        0.0
    } else {
        2.0 * recall * clean / (recall + clean)
    };
    times.sort_by(f64::total_cmp);
    let q = |p: f64| {
        times
            .get(((times.len() as f64 - 1.0) * p).round() as usize)
            .copied()
            .unwrap_or(0.0)
    };

    println!(
        "pages with truth phrases: {labeled}, with HTML snapshot: {fetched}, PDF (not evaluated): {pdfs}"
    );
    println!();
    println!("crw metric (truth phrase hit >= 30%)");
    println!(
        "  found / all labeled pages:    {found} / {labeled} = {:.2}%",
        pct(found, labeled)
    );
    println!(
        "  found / pages with snapshot:  {found_fetched} / {fetched} = {:.2}%",
        pct(found_fetched, fetched)
    );
    println!();
    println!("strict metric (pages with snapshot)");
    println!("  mean truth recall:   {:.3}", recall);
    println!(
        "  mean noise leak:     {:.3}  ({leak_pages} pages with noise phrases)",
        leak
    );
    println!("  content score (F1 of recall and 1 - leak): {:.3}", score);
    println!();
    println!(
        "speed (process only, best of {} runs, mean page {} KB)",
        args.runs.max(1),
        bytes_total.checked_div(fetched).unwrap_or(0) / 1024
    );
    println!(
        "  p50 {:.2} ms   p95 {:.2} ms   max {:.2} ms   total {:.0} ms",
        q(0.5),
        q(0.95),
        times.last().copied().unwrap_or(0.0),
        times.iter().sum::<f64>()
    );
    Ok(())
}

fn is_pdf(bytes: &[u8], content_type: Option<&str>) -> bool {
    bytes.starts_with(b"%PDF-")
        || content_type.is_some_and(|c| c.to_ascii_lowercase().contains("application/pdf"))
}

fn read_rows(path: &Path) -> Result<Vec<Row>> {
    let mut reader =
        csv::Reader::from_path(path).with_context(|| format!("reading {}", path.display()))?;
    reader
        .deserialize()
        .collect::<Result<Vec<Row>, _>>()
        .with_context(|| format!("parsing {}", path.display()))
}

fn read_metas(path: &Path) -> Result<HashMap<String, Meta>> {
    let file = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut map = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let meta: Meta =
            serde_json::from_str(&line).with_context(|| format!("parsing {}", path.display()))?;
        map.insert(meta.id.clone(), meta);
    }
    Ok(map)
}

/// 字符集：BOM > Content-Type 头 > 文档开头的 `<meta charset>` > UTF-8。
/// 正式的字符集识别在网络层（M3）实现，这里够评测用。
fn decode(bytes: &[u8], content_type: Option<&str>) -> String {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom_len..])
            .0
            .into_owned();
    }
    let label = content_type
        .and_then(charset_param)
        .or_else(|| sniff_meta_charset(&bytes[..bytes.len().min(4096)]));
    let enc = label
        .and_then(|l| Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches(['"', '\'']).to_string())
    })
}

fn sniff_meta_charset(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let i = text.find("charset=")? + "charset=".len();
    let value: String = text[i..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_follow_crw_rule() {
        let p = phrases(
            "short\n  exactly twenty chars  \nthis line is longer than twenty\n",
            20,
        );
        assert_eq!(p, vec!["this line is longer than twenty"]);
    }

    #[test]
    fn decodes_by_header_and_meta() {
        let gbk = encoding_rs::GBK.encode("中文").0.into_owned();
        assert_eq!(decode(&gbk, Some("text/html; charset=GBK")), "中文");
        let mut doc = b"<meta charset=\"gb2312\">".to_vec();
        doc.extend_from_slice(&gbk);
        assert!(decode(&doc, Some("text/html")).ends_with("中文"));
        assert_eq!(decode("plain".as_bytes(), None), "plain");
    }
}
