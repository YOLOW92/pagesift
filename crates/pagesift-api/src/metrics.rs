//! `/metrics` 的计数器和直方图（Prometheus 文本格式）。不引入指标库：路由、错误码这些标签的取值
//! 都是有限集合，几把短锁加原子计数就够了。

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

/// 请求延迟直方图的桶上界（秒）。
const BUCKETS: [f64; 14] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

#[derive(Default, Clone)]
struct Histogram {
    /// 落在每个桶里的次数（不累计），最后一个是 +Inf。
    counts: [u64; BUCKETS.len() + 1],
    sum: f64,
}

impl Histogram {
    fn observe(&mut self, secs: f64) {
        let i = BUCKETS
            .iter()
            .position(|b| secs <= *b)
            .unwrap_or(BUCKETS.len());
        self.counts[i] += 1;
        self.sum += secs;
    }
}

/// 页面从哪来：单页抓取、批量抓取、整站爬取。
#[derive(Debug, Clone, Copy)]
pub(crate) enum PageSource {
    Scrape,
    Batch,
    Crawl,
}

#[derive(Default)]
pub(crate) struct Metrics {
    requests: Mutex<BTreeMap<(String, u16), u64>>,
    latency: Mutex<BTreeMap<String, Histogram>>,
    in_flight: AtomicI64,
    pub(crate) scrape_ok: AtomicU64,
    pub(crate) scrape_err: AtomicU64,
    pages: [AtomicU64; 3],
    /// (错误码, 来源)：`http` 是请求本身的错误响应，`job` 是任务里单个页面的错误。
    errors: Mutex<BTreeMap<(&'static str, &'static str), u64>>,
    jobs_started: Mutex<BTreeMap<&'static str, u64>>,
    webhooks: Mutex<BTreeMap<&'static str, u64>>,
}

fn bump<K: Ord>(m: &Mutex<BTreeMap<K, u64>>, k: K, n: u64) {
    *m.lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(k)
        .or_default() += n;
}

impl Metrics {
    pub(crate) fn request_started(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn request_finished(&self, route: &str, status: u16, elapsed: Duration) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        bump(&self.requests, (route.to_string(), status), 1);
        self.latency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(route.to_string())
            .or_default()
            .observe(elapsed.as_secs_f64());
    }

    pub(crate) fn page(&self, source: PageSource) {
        self.pages[source as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn error(&self, code: &'static str, source: &'static str) {
        bump(&self.errors, (code, source), 1);
    }

    pub(crate) fn job_started(&self, kind: &'static str) {
        bump(&self.jobs_started, kind, 1);
    }

    /// webhook 投递结果：`delivered`、`retried`、`failed`、`blocked`、`dropped`。
    pub(crate) fn webhook(&self, outcome: &'static str) {
        bump(&self.webhooks, outcome, 1);
    }

    /// 输出 Prometheus 文本格式；`jobs` 是当前任务数，按 (类型, 状态) 分组。
    pub(crate) fn render(&self, jobs: &BTreeMap<(&'static str, &'static str), u64>) -> String {
        let mut out = String::new();

        header(
            &mut out,
            "pagesift_http_requests_total",
            "counter",
            "HTTP requests by route and status.",
        );
        for ((route, status), n) in snapshot(&self.requests) {
            let _ = writeln!(
                out,
                "pagesift_http_requests_total{{route=\"{route}\",status=\"{status}\"}} {n}"
            );
        }

        header(
            &mut out,
            "pagesift_http_request_duration_seconds",
            "histogram",
            "HTTP request latency by route.",
        );
        let latency = self
            .latency
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for (route, h) in latency {
            let mut cum = 0;
            for (i, b) in BUCKETS.iter().enumerate() {
                cum += h.counts[i];
                let _ = writeln!(
                    out,
                    "pagesift_http_request_duration_seconds_bucket{{route=\"{route}\",le=\"{b}\"}} {cum}"
                );
            }
            cum += h.counts[BUCKETS.len()];
            let _ = writeln!(
                out,
                "pagesift_http_request_duration_seconds_bucket{{route=\"{route}\",le=\"+Inf\"}} {cum}"
            );
            let _ = writeln!(
                out,
                "pagesift_http_request_duration_seconds_sum{{route=\"{route}\"}} {}",
                h.sum
            );
            let _ = writeln!(
                out,
                "pagesift_http_request_duration_seconds_count{{route=\"{route}\"}} {cum}"
            );
        }

        header(
            &mut out,
            "pagesift_http_requests_in_flight",
            "gauge",
            "HTTP requests being served.",
        );
        let _ = writeln!(
            out,
            "pagesift_http_requests_in_flight {}",
            self.in_flight.load(Ordering::Relaxed)
        );

        header(
            &mut out,
            "pagesift_scrapes_total",
            "counter",
            "Single-page scrapes by outcome.",
        );
        let _ = writeln!(
            out,
            "pagesift_scrapes_total{{outcome=\"ok\"}} {}",
            self.scrape_ok.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "pagesift_scrapes_total{{outcome=\"error\"}} {}",
            self.scrape_err.load(Ordering::Relaxed)
        );

        header(
            &mut out,
            "pagesift_pages_scraped_total",
            "counter",
            "Pages scraped successfully, by endpoint.",
        );
        for (i, source) in ["scrape", "batch_scrape", "crawl"].iter().enumerate() {
            let _ = writeln!(
                out,
                "pagesift_pages_scraped_total{{source=\"{source}\"}} {}",
                self.pages[i].load(Ordering::Relaxed)
            );
        }

        header(
            &mut out,
            "pagesift_errors_total",
            "counter",
            "Errors by code; source is http (request) or job (page in a job).",
        );
        for ((code, source), n) in snapshot(&self.errors) {
            let _ = writeln!(
                out,
                "pagesift_errors_total{{code=\"{code}\",source=\"{source}\"}} {n}"
            );
        }

        header(
            &mut out,
            "pagesift_jobs_started_total",
            "counter",
            "Crawl and batch scrape jobs started.",
        );
        for (kind, n) in snapshot(&self.jobs_started) {
            let _ = writeln!(out, "pagesift_jobs_started_total{{kind=\"{kind}\"}} {n}");
        }

        header(
            &mut out,
            "pagesift_jobs",
            "gauge",
            "Jobs kept by the server, by kind and status.",
        );
        for ((kind, status), n) in jobs {
            let _ = writeln!(
                out,
                "pagesift_jobs{{kind=\"{kind}\",status=\"{status}\"}} {n}"
            );
        }

        header(
            &mut out,
            "pagesift_webhook_deliveries_total",
            "counter",
            "Webhook delivery attempts by outcome.",
        );
        for (outcome, n) in snapshot(&self.webhooks) {
            let _ = writeln!(
                out,
                "pagesift_webhook_deliveries_total{{outcome=\"{outcome}\"}} {n}"
            );
        }
        out
    }
}

fn snapshot<K: Ord + Clone>(m: &Mutex<BTreeMap<K, u64>>) -> BTreeMap<K, u64> {
    m.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn header(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_are_cumulative() {
        let m = Metrics::default();
        for ms in [0, 3, 3, 200, 60_000] {
            m.request_started();
            m.request_finished("/v2/scrape", 200, Duration::from_millis(ms));
        }
        let out = m.render(&BTreeMap::new());
        assert!(
            out.contains("route=\"/v2/scrape\",le=\"0.001\"} 1\n"),
            "{out}"
        );
        assert!(
            out.contains("route=\"/v2/scrape\",le=\"0.005\"} 3\n"),
            "{out}"
        );
        assert!(out.contains("route=\"/v2/scrape\",le=\"30\"} 4\n"), "{out}");
        assert!(
            out.contains("route=\"/v2/scrape\",le=\"+Inf\"} 5\n"),
            "{out}"
        );
        assert!(
            out.contains("pagesift_http_request_duration_seconds_count{route=\"/v2/scrape\"} 5\n")
        );
        assert!(out.contains("pagesift_http_requests_in_flight 0\n"));
    }
}
