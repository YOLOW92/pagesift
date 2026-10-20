//! `pagesift bench`：在本地快照上评测正文提取质量和速度。
//!
//! 数据集是 Firecrawl 的 scrape-content-dataset-v1（`truth_text` 是正文片段，
//! `lie_text` 是导航、页脚这类噪声片段），快照由 `bench/fetch_snapshots.py` 抓取。
//! 两个质量口径：
//! - crw 口径：`truth_text` 按行切开、长度超过 20 字符的行算短语，命中 ≥ 30% 算"找到"；
//! - 严格口径：正文召回率（命中短语比例的平均）和噪声泄漏率（`lie_text` 短语漏进输出的比例的平均），
//!   两者合成一个综合分。

use std::collections::{HashMap, HashSet};
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
    /// 模拟 `engine: auto`：HTTP 快照的正文太少、又有渲染快照时，改用渲染快照。
    pub auto: bool,
    /// 把每页最终使用的 HTML（统一为 UTF-8）和清单写到这个目录，供其他工具做对照评测。
    pub emit_inputs: Option<PathBuf>,
    /// 不跑 pagesift，改为给其他工具的输出打分（JSON lines：id、markdown、ms）。
    pub external: Option<PathBuf>,
    /// 吞吐量测试的线程数；0 表示用上所有核。
    pub jobs: usize,
}

#[derive(Deserialize)]
struct ExternalOutput {
    id: String,
    #[serde(default)]
    markdown: String,
    #[serde(default)]
    ms: Option<f64>,
}

use pagesift::AUTO_RENDER_MIN_CHARS;

/// 严格口径的规范化，短语和输出用同一套：`&nbsp;` 当空格，去掉 Markdown 的格式符号
/// （强调、行内代码、表格竖线），空白折叠成一个空格。数据集的正文片段保留了网页源码里的
/// 连续空格和制表符，而任何 Markdown 输出都会折叠空白、加格式符号；不规范化的话，
/// 内容明明在输出里也算没找到。对所有工具一视同仁。
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if matches!(c, '*' | '_' | '`' | '|') {
            continue;
        }
        if c.is_whitespace() || c == '\u{a0}' {
            if !space && !out.is_empty() {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// 短噪声短语（不超过这么多个词）要作为独立单元出现才算泄漏。
const SHORT_NOISE_WORDS: usize = 3;

/// 噪声短语是否漏进了输出。长短语按子串算；一到三个词的短语（菜单项、按钮文字）只有在输出里
/// 作为独立的一行、列表项、链接文字或表格单元格出现才算：导航项 "Attorneys" 出现在正文句子
/// "…Requiring Attorneys to Complete…" 里、"Advertise" 出现在 "Advertisement" 里都不是泄漏。
fn leaked(phrase: &str, hay: &str, units: &HashSet<String>) -> bool {
    let p = normalize(phrase);
    if p.split(' ').count() > SHORT_NOISE_WORDS {
        return hay.contains(p.as_str());
    }
    units.contains(&p)
}

/// 输出里的独立文本单元（规范化后）：每一行去掉列表、标题、引用标记后的文字，每个链接文字，
/// 每个表格单元格。
fn text_units(markdown: &str) -> HashSet<String> {
    let link = Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("valid regex");
    let marker = Regex::new(r"^\s*(?:[-*+]|\d+[.)]|#{1,6}|>)\s+").expect("valid regex");
    let mut units = HashSet::new();
    for line in markdown.lines() {
        for cap in link.captures_iter(line) {
            units.insert(normalize(&cap[1].to_lowercase()));
        }
        let plain = link.replace_all(line, "$1");
        let plain = marker.replace(&plain, "");
        if plain.contains('|') {
            for cell in plain.split('|') {
                units.insert(normalize(&cell.to_lowercase()));
            }
        }
        units.insert(normalize(&plain.to_lowercase()));
    }
    units.remove("");
    units
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
    let rendered_path = args.dir.join("snapshots/rendered.jsonl");
    let rendered = if args.auto && rendered_path.exists() {
        read_metas(&rendered_path)?
    } else {
        HashMap::new()
    };
    let mut rendered_used = 0usize;
    let external: Option<HashMap<String, ExternalOutput>> = match &args.external {
        Some(p) => {
            let file = File::open(p).with_context(|| format!("reading {}", p.display()))?;
            let mut map = HashMap::new();
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let o: ExternalOutput = serde_json::from_str(&line)
                    .with_context(|| format!("parsing {}", p.display()))?;
                map.insert(o.id.clone(), o);
            }
            Some(map)
        }
        None => None,
    };
    let mut manifest = match &args.emit_inputs {
        Some(d) => {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
            Some(BufWriter::new(File::create(d.join("manifest.jsonl"))?))
        }
        None => None,
    };
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
    let mut workload: Vec<(String, Options)> = Vec::new();
    let mut bytes_total = 0usize;
    let mut pdfs = 0usize;

    for row in rows.iter().take(args.limit.unwrap_or(usize::MAX)) {
        let truth = phrases(&row.truth_text, TRUTH_MIN_CHARS);
        if truth.is_empty() {
            continue;
        }
        labeled += 1;
        // 噪声短语如果也出现在正文里（目录里的章节名同时是正文的小标题），保留它才是对的，
        // 不能算泄漏；评测集里约 7% 的噪声短语是这种情况
        let truth_norm = normalize(&row.truth_text.to_lowercase());
        let lies: Vec<String> = phrases(&row.lie_text, LIE_MIN_CHARS)
            .into_iter()
            .filter(|p| !truth_norm.contains(normalize(p).as_str()))
            .collect();
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
                ..Default::default()
            };
            let (html, opts) = match rendered.get(&row.id) {
                Some(r)
                    if r.error.is_none()
                        && process(&html, &opts).markdown.chars().count()
                            < AUTO_RENDER_MIN_CHARS =>
                {
                    match std::fs::read_to_string(
                        args.dir.join(format!("snapshots/{}.rendered.html", row.id)),
                    ) {
                        Ok(rendered_html) => {
                            rendered_used += 1;
                            let base = r
                                .final_url
                                .as_deref()
                                .and_then(|u| Url::parse(u).ok())
                                .or(opts.base_url.clone());
                            (
                                rendered_html,
                                Options {
                                    base_url: base,
                                    ..opts
                                },
                            )
                        }
                        Err(_) => (html, opts),
                    }
                }
                _ => (html, opts),
            };
            if let (Some(m), Some(dir)) = (manifest.as_mut(), args.emit_inputs.as_ref()) {
                let path = dir.join(format!("{}.html", row.id));
                std::fs::write(&path, &html)?;
                serde_json::to_writer(
                    &mut *m,
                    &serde_json::json!({
                        "id": row.id,
                        "url": row.url,
                        "base_url": opts.base_url.as_ref().map(Url::as_str),
                        "path": path,
                    }),
                )?;
                m.write_all(b"\n")?;
            }
            let mut best = f64::MAX;
            let mut markdown = String::new();
            if let Some(ext) = external.as_ref() {
                if let Some(o) = ext.get(&row.id) {
                    markdown = o.markdown.clone();
                    best = o.ms.unwrap_or(0.0);
                } else {
                    best = 0.0;
                }
            } else {
                for _ in 0..args.runs.max(1) {
                    let t = Instant::now();
                    let doc = process(&html, &opts);
                    best = best.min(t.elapsed().as_secs_f64() * 1000.0);
                    markdown = doc.markdown;
                }
                workload.push((html.clone(), opts.clone()));
            }
            times.push(best);
            result.process_ms = (best * 1000.0).round() / 1000.0;
            result.markdown_chars = markdown.chars().count();

            let hay =
                format!("{markdown}\n{}", link_re.replace_all(&markdown, "$1")).to_lowercase();
            // crw 口径照它公开的规则：原样子串匹配
            let raw_hits = truth.iter().filter(|p| hay.contains(p.as_str())).count();
            result.found = raw_hits as f64 / truth.len() as f64 >= FOUND_THRESHOLD;
            // 严格口径：两边做同样的规范化再比（见 normalize），只看内容在不在
            let hay = normalize(&hay);
            result.truth_hit = truth
                .iter()
                .filter(|p| hay.contains(normalize(p).as_str()))
                .count();
            result.truth_recall = result.truth_hit as f64 / truth.len() as f64;
            if !lies.is_empty() {
                let units = text_units(&markdown);
                result.lie_hit = lies.iter().filter(|p| leaked(p, &hay, &units)).count();
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
    if let Some(mut m) = manifest {
        m.flush()?;
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
    if args.auto {
        println!("engine auto: {rendered_used} pages used a browser-rendered snapshot");
    }
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
    // 大页面先处理，避免最后剩一个大页面拖长墙钟时间
    workload.sort_by_key(|(html, _)| std::cmp::Reverse(html.len()));
    if !workload.is_empty() {
        let threads = if args.jobs == 0 {
            std::thread::available_parallelism().map_or(1, usize::from)
        } else {
            args.jobs
        };
        let wall = (0..2)
            .map(|_| throughput(&workload, threads))
            .fold(f64::MAX, f64::min);
        println!(
            "  throughput: {threads} threads, {:.0} pages/s ({} pages in {:.0} ms wall)",
            workload.len() as f64 / (wall / 1000.0),
            workload.len(),
            wall
        );
    }
    Ok(())
}

/// 多线程处理全部页面，返回墙钟耗时（毫秒）。
fn throughput(workload: &[(String, Options)], threads: usize) -> f64 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let start = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((html, opts)) = workload.get(i) else {
                        break;
                    };
                    std::hint::black_box(process(html, opts));
                }
            });
        }
    });
    start.elapsed().as_secs_f64() * 1000.0
}

/// 为 `bench --auto` 准备渲染快照：HTTP 快照上正文少于 [`AUTO_RENDER_MIN_CHARS`] 的页面，
/// 用浏览器重新渲染一次。只访问已经成功抓过（robots 允许）的页面；可以中断后续跑。
pub fn render_snapshots(dir: &Path, limit: Option<usize>, concurrency: usize) -> Result<()> {
    use std::sync::Arc;
    use std::time::Duration;

    use pagesift_cdp::{
        BlockResources, BrowserPool, PoolConfig, PrivateNetworkGuard, RenderOptions,
    };

    let rows = read_rows(&dir.join("1-0-0.csv"))?;
    let metas = read_metas(&dir.join("snapshots/meta.jsonl"))?;
    let log_path = dir.join("snapshots/rendered.jsonl");
    let done: std::collections::HashSet<String> = if log_path.exists() {
        read_metas(&log_path)?.into_keys().collect()
    } else {
        Default::default()
    };

    let mut todo = Vec::new();
    for row in rows.iter().take(limit.unwrap_or(usize::MAX)) {
        let Some(meta) = metas.get(&row.id) else {
            continue;
        };
        if meta.error.is_some() || done.contains(&row.id) {
            continue;
        }
        let Ok(bytes) = std::fs::read(dir.join(format!("snapshots/{}.html", row.id))) else {
            continue;
        };
        if is_pdf(&bytes, meta.content_type.as_deref()) {
            continue;
        }
        let html = decode(&bytes, meta.content_type.as_deref());
        let md = process(&html, &Options::default()).markdown;
        if md.chars().count() < AUTO_RENDER_MIN_CHARS {
            todo.push((row.id.clone(), row.url.clone()));
        }
    }
    println!(
        "{} pages to render ({} already done)",
        todo.len(),
        done.len()
    );

    let runtime = tokio::runtime::Runtime::new().context("starting tokio")?;
    runtime.block_on(async move {
        let cfg = PoolConfig {
            max_browsers: concurrency.div_ceil(4).max(1),
            max_tabs_per_browser: 4,
            ..PoolConfig::default()
        };
        let pool = Arc::new(BrowserPool::new(cfg, Arc::new(PrivateNetworkGuard)).await?);
        let log = Arc::new(std::sync::Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?,
        ));
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let opts = RenderOptions {
            wait_for: Duration::from_millis(1500),
            timeout: Duration::from_secs(45),
            block: BlockResources {
                ads: true,
                fonts: true,
                media: true,
                images: true,
            },
            ..RenderOptions::default()
        };
        let total = todo.len();
        let mut tasks = tokio::task::JoinSet::new();
        for (id, url) in todo {
            let (pool, log, sem, opts, dir) = (
                pool.clone(),
                log.clone(),
                sem.clone(),
                opts.clone(),
                dir.to_path_buf(),
            );
            tasks.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore open");
                let t = Instant::now();
                let mut record = serde_json::json!({ "id": id, "url": url });
                match pool.render(&url, &opts).await {
                    Ok(page) => {
                        let path = dir.join(format!("snapshots/{id}.rendered.html"));
                        if let Err(e) = std::fs::write(&path, &page.html) {
                            record["error"] = e.to_string().into();
                        }
                        record["final_url"] = page.final_url.into();
                        record["status"] = page.status.into();
                    }
                    Err(e) => {
                        record["error"] = e.to_string().chars().take(200).collect::<String>().into()
                    }
                }
                record["render_ms"] = (t.elapsed().as_millis() as u64).into();
                let mut f = log.lock().expect("log lock");
                let _ = writeln!(f, "{record}");
            });
        }
        let mut finished = 0;
        while tasks.join_next().await.is_some() {
            finished += 1;
            if finished % 25 == 0 || finished == total {
                println!("  {finished}/{total}");
            }
        }
        pool.shutdown().await;
        anyhow::Ok(())
    })
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
