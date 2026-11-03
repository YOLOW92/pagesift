//! 0.95 监控评测：在虚拟时间里重放每个网址的变化时间表（`benchmarks/history/0.95/gen.py`），
//! 比较服务端用的自适应间隔（伽马后验 + δ，`pagesift_stat::poisson`，最短 5 分钟）和请求总数相同的
//! 固定间隔轮询。每次检查把当时的页面转成 Markdown，和上次检查的 Markdown 做块级、区分噪声的 diff
//! （`pagesift_diff`，和监控服务同一个判断）：实质变化才算通知。
//!
//! 指标：误报率（通知里，上次检查以来其实只有噪声变化的比例，↓）、漏报率（实质变化在下一次检查时
//! 没被报出来的比例，↓）、平均发现延迟（↓）、每发现一次变化的请求数（↓）。
//!
//!     cargo run --release --example monitor_sim -- benchmarks/data/0.95/timelines.jsonl

use std::io::BufRead;

use pagesift_stat::poisson::ChangeRate;

const DELTA: f64 = 0.05; // [δ]
const MIN_INTERVAL: f64 = 300.0; // [协议] Firecrawl 的最短间隔

struct Page {
    html: String,
    events: Vec<(f64, bool)>, // (时刻, 是否实质)
    horizon: f64,
}

/// 时刻 t 的页面：t 之前发生了多少次实质、噪声变化决定页面内容。
fn render(p: &Page, t: f64) -> String {
    let (mut subst, mut noise) = (0usize, 0usize);
    for (et, s) in &p.events {
        if *et > t {
            break;
        }
        if *s { subst += 1 } else { noise += 1 }
    }
    let mut body = String::new();
    for k in 0..subst {
        body.push_str(&format!(
            "<p>Update {k}: the team published new findings number {k} with a detailed explanation of what changed and why it matters for readers.</p>"
        ));
    }
    let ads = [
        "Spring sale on garden tools",
        "New phones in stock",
        "Travel insurance deals",
        "Learn a language today",
    ];
    let noise_block = format!(
        "<div id='ps-noise'><p>Updated 2026-11-{:02} 0{}:00 · {} views</p><p>Sponsored: {}</p></div>",
        1 + noise % 28,
        noise % 10,
        1000 + 37 * noise,
        ads[noise % ads.len()]
    );
    // 改动放进正文里：第一个段落之后（正文识别只保留正文，放在页尾的改动会被当作页面杂项去掉）
    let insert = format!("{noise_block}{body}");
    match first_paragraph_end(&p.html) {
        Some(i) => format!("{}{insert}{}", &p.html[..i], &p.html[i..]),
        None => format!("{}{insert}", p.html),
    }
}

/// 正文区域（`<main>`、`<article>`，没有时整页）里第一个 `</p>` 之后的位置。
fn first_paragraph_end(html: &str) -> Option<usize> {
    let start = html
        .find("<main")
        .or_else(|| html.find("<article"))
        .unwrap_or(0);
    html[start..].find("</p>").map(|i| start + i + 4)
}

fn markdown(html: &str) -> String {
    pagesift_content::process(html, &Default::default()).markdown
}

#[derive(Default, Debug)]
struct Stats {
    requests: usize,
    notified: usize,
    false_alarms: usize,
    substantive: usize,
    detected: usize,
    delay_sum: f64,
}

/// 用给定的“下一次间隔”策略跑一个网址。
fn simulate(p: &Page, mut next: impl FnMut(&mut ChangeRate, f64, bool) -> f64, st: &mut Stats) {
    let mut rate = ChangeRate::default();
    let mut t = 0.0;
    let mut prev_md = markdown(&render(p, t));
    st.requests += 1;
    let mut last = t;
    let mut i = 0; // 下一个还没被检查覆盖的事件
    st.substantive += p.events.iter().filter(|e| e.1).count();
    let mut dt = next(&mut rate, 0.0, false);
    loop {
        t = last + dt;
        if t > p.horizon {
            break;
        }
        st.requests += 1;
        let md = markdown(&render(p, t));
        let report = pagesift_diff::diff_with(&prev_md, &md, &noise());
        let notified = report.substantive;
        let mut had_substantive = false;
        while i < p.events.len() && p.events[i].0 <= t {
            if p.events[i].1 {
                had_substantive = true;
                if notified {
                    st.detected += 1;
                    st.delay_sum += t - p.events[i].0;
                }
            }
            i += 1;
        }
        if notified {
            st.notified += 1;
            if !had_substantive {
                st.false_alarms += 1;
                if std::env::var_os("PS_DEBUG").is_some() && st.false_alarms <= 3 {
                    for b in report
                        .blocks
                        .iter()
                        .filter(|b| b.significance == pagesift_diff::Significance::Substantive)
                    {
                        eprintln!(
                            "FA {:?} old={:?} new={:?}",
                            b.change, b.old_text, b.new_text
                        );
                    }
                }
            }
        }
        prev_md = md;
        dt = next(&mut rate, t - last, notified);
        last = t;
    }
}

/// 和监控服务相同的噪声类别（`pagesift-api/src/monitor/run.rs` 的 `monitor_noise`）。
fn noise() -> pagesift_diff::DiffOptions {
    use pagesift_diff::NoiseCategory::*;
    pagesift_diff::DiffOptions {
        noise_categories: vec![Date, RelativeTime, Count, Advertisement],
        ..Default::default()
    }
}

/// 所有网址在一个监控里：每次检查后按服务端的规则重新分配（`pagesift-api/src/monitor/run.rs` 的 `allocate`）。
fn simulate_allocated(pages: &[Page]) -> Stats {
    let mut st = Stats::default();
    let n = pages.len();
    let mut rate = vec![ChangeRate::default(); n];
    let mut last = vec![0.0f64; n];
    let mut prev: Vec<String> = pages.iter().map(|p| markdown(&render(p, 0.0))).collect();
    let mut idx = vec![0usize; n];
    st.requests += n;
    st.substantive += pages
        .iter()
        .map(|p| p.events.iter().filter(|e| e.1).count())
        .sum::<usize>();
    let mut cache: Vec<(f64, f64)> = rate
        .iter()
        .map(|r| (r.mean(), r.next_check(DELTA)))
        .collect();
    let due = |cache: &[(f64, f64)], last: &[f64]| -> Vec<f64> {
        let budget: f64 = cache.iter().map(|c| 1.0 / c.1.max(MIN_INTERVAL)).sum();
        let s: f64 = cache.iter().map(|c| c.0.sqrt()).sum();
        cache
            .iter()
            .zip(last)
            .map(|(c, l)| l + (s / (budget * c.0.sqrt())).max(MIN_INTERVAL))
            .collect()
    };
    loop {
        let d = due(&cache, &last);
        let (i, t) = d
            .iter()
            .copied()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let p = &pages[i];
        if t > p.horizon {
            break;
        }
        st.requests += 1;
        let md = markdown(&render(p, t));
        let rep = pagesift_diff::diff_with(&prev[i], &md, &noise());
        let notified = rep.substantive;
        let mut had = false;
        while idx[i] < p.events.len() && p.events[idx[i]].0 <= t {
            if p.events[idx[i]].1 {
                had = true;
                if notified {
                    st.detected += 1;
                    st.delay_sum += t - p.events[idx[i]].0;
                }
            }
            idx[i] += 1;
        }
        if notified {
            st.notified += 1;
            if !had {
                st.false_alarms += 1;
                if std::env::var_os("PS_DEBUG").is_some() {
                    for b in rep
                        .blocks
                        .iter()
                        .filter(|b| b.significance == pagesift_diff::Significance::Substantive)
                    {
                        eprintln!(
                            "FA {i} {:?} old={:?} new={:?}",
                            b.change,
                            b.old_text
                                .as_deref()
                                .map(|t| t.chars().take(90).collect::<String>()),
                            b.new_text
                                .as_deref()
                                .map(|t| t.chars().take(90).collect::<String>())
                        );
                    }
                }
            }
        }
        prev[i] = md;
        rate[i].observe(t - last[i], notified);
        cache[i] = (rate[i].mean(), rate[i].next_check(DELTA));
        last[i] = t;
    }
    st
}

fn report(name: &str, s: &Stats) -> serde_json::Value {
    let f = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    serde_json::json!({
        "policy": name,
        "requests": s.requests,
        "notifications": s.notified,
        "false_alarm_rate": f(s.false_alarms, s.notified),
        "miss_rate": f(s.substantive - s.detected, s.substantive),
        "mean_delay_hours": if s.detected == 0 { 0.0 } else { s.delay_sum / s.detected as f64 / 3600.0 },
        "requests_per_detected_change": f(s.requests, s.detected),
        "substantive_changes": s.substantive,
    })
}

fn main() {
    let path = std::env::args().nth(1).expect("timelines.jsonl");
    let pages: Vec<Page> = std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(&l.unwrap()).unwrap();
            Page {
                html: v["html"].as_str().unwrap().to_string(),
                horizon: v["horizon"].as_f64().unwrap(),
                events: v["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| (e["t"].as_f64().unwrap(), e["kind"] == "substantive"))
                    .collect(),
            }
        })
        .collect();
    // 自适应：和服务端一样，观测后取 P(漏掉变化) = δ 的时刻，不短于最短间隔
    let mut adaptive = Stats::default();
    for p in &pages {
        simulate(
            p,
            |rate, dt, changed| {
                if dt > 0.0 {
                    rate.observe(dt, changed);
                }
                rate.next_check(DELTA).max(MIN_INTERVAL)
            },
            &mut adaptive,
        );
    }
    // 自适应 + 预算分配：δ 规则定总请求率，在各网址之间按 T_i ∝ λ_i^{-1/2} 分（和服务端一样）
    let alloc = simulate_allocated(&pages);
    // 固定间隔：总请求数和自适应相同
    let horizon: f64 = pages.iter().map(|p| p.horizon).sum();
    let interval = horizon / alloc.requests.saturating_sub(pages.len()).max(1) as f64;
    let mut fixed = Stats::default();
    for p in &pages {
        simulate(p, |_, _, _| interval, &mut fixed);
    }
    let out = serde_json::json!({
        "pages": pages.len(),
        "fixed_interval_hours": interval / 3600.0,
        "adaptive_per_url": report("adaptive_per_url", &adaptive),
        "adaptive": report("adaptive", &alloc),
        "fixed": report("fixed", &fixed),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
