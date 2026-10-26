//! 审计日志：每次抓取写一行 JSON（只有元数据），结果可疑时另存当时的 HTML，供事后复现。
//!
//! 单进程长期运行的服务（`serve`、`mcp http`）用它当真实流量的测试场：抓了什么、走了哪个引擎、
//! 结果怎样、哪里不对，都能从日志里按站点和调用方汇总，可疑页面能用快照在本地反复重放。
//!
//! - 日志按大小轮转：超过上限就把当前文件改名为 `<path>.1`（覆盖旧的）再新开，磁盘占用不超过两份上限。
//! - URL 默认去掉用户名密码，查询参数里像凭证的值（token、key、signature……）换成 `REDACTED`。
//! - 快照只在结果可疑时写，每天有条数上限，单个文件有大小上限。

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_net::Url;
use serde_json::{Value, json};

tokio::task_local! {
    /// 发起这次抓取的客户端（MCP 的 clientInfo 名称等），只用于审计日志。
    pub static CLIENT: String;
}

/// 审计日志配置。
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// JSON Lines 日志文件。
    pub path: PathBuf,
    /// 当前日志文件超过这个大小就轮转。
    pub max_bytes: u64,
    /// 结果可疑时保存 HTML 快照的目录；`None` 表示不存快照。
    pub snapshot_dir: Option<PathBuf>,
    /// 单个快照最多保存多少字节，超出部分截掉。
    pub snapshot_max_bytes: usize,
    /// 每天最多保存多少个快照。
    pub snapshots_per_day: u32,
    /// URL 里的凭证是否脱敏。
    pub redact: bool,
    /// 运行方式（`serve`、`mcp-http`、`scrape`……），写进每条记录。
    pub source: String,
}

impl AuditConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let snapshot_dir = path.parent().map(|p| p.join("snapshots"));
        Self {
            path,
            max_bytes: 64 * 1024 * 1024,
            snapshot_dir,
            snapshot_max_bytes: 2 * 1024 * 1024,
            snapshots_per_day: 200,
            redact: true,
            source: "pagesift".into(),
        }
    }
}

struct LogFile {
    file: File,
    len: u64,
}

pub struct Audit {
    cfg: AuditConfig,
    log: Mutex<LogFile>,
    /// 快照计数所在的那一天（Unix 天数）和当天已存的个数。
    snap_day: AtomicU64,
    snap_count: AtomicU32,
}

/// 这一类问题值得留快照：页面抓到了，但正文可能丢了。
const SUSPICIOUS_WARNINGS: &[&str] = &[
    "empty_content",
    "thin_content",
    "partial_content",
    "replies_omitted",
    "render_unavailable",
    "render_skipped_",
];
/// 抓到 200 的 HTML 页面却少于这么多字，也算可疑。
const SHORT_TEXT_CHARS: usize = 200;

impl Audit {
    pub fn open(cfg: AuditConfig) -> io::Result<Self> {
        if let Some(dir) = cfg.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cfg.path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            cfg,
            log: Mutex::new(LogFile { file, len }),
            snap_day: AtomicU64::new(0),
            snap_count: AtomicU32::new(0),
        })
    }

    /// 快照要用渲染或抓取得到的原始 HTML：开着快照时内部多要一份 rawHtml。
    pub(crate) fn wants_raw_html(&self) -> bool {
        self.cfg.snapshot_dir.is_some()
    }

    /// 记一次抓取。`raw_html` 是这次得到的 HTML（缓存命中、出错时没有）。
    pub(crate) fn record_scrape(
        &self,
        kind: &str,
        url: &str,
        opts: &ScrapeOptions,
        result: &Result<Document, ApiError>,
        raw_html: Option<&str>,
        elapsed: Duration,
    ) {
        let mut rec = self.base(kind);
        rec["url"] = json!(self.clean_url(url));
        rec["engineRequested"] = json!(opts.engine.as_deref().unwrap_or("auto"));
        rec["ms"] = json!((elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0);
        let mut suspicious: Option<String> = None;
        match result {
            Ok(doc) => {
                let m = &doc.metadata;
                let p = m.pagesift.as_ref();
                if let Some(u) = m.url.as_deref().filter(|u| *u != url) {
                    rec["finalUrl"] = json!(self.clean_url(u));
                }
                rec["status"] = json!(m.status_code);
                rec["contentType"] = json!(m.content_type);
                rec["engine"] = json!(p.and_then(|p| p.engine.as_deref()));
                rec["browserEngine"] = json!(p.and_then(|p| p.browser_engine.as_deref()));
                rec["cache"] = json!(m.cache_state);
                let chars = doc
                    .markdown
                    .as_deref()
                    .map(|md| md.chars().filter(|c| !c.is_whitespace()).count());
                rec["textChars"] = json!(chars);
                rec["links"] = json!(doc.links.as_ref().map(Vec::len));
                if let Some(b) = raw_html.map(str::len) {
                    rec["htmlBytes"] = json!(b);
                }
                let warnings: &[String] = p.map(|p| p.warnings.as_slice()).unwrap_or_default();
                if !warnings.is_empty() {
                    rec["warnings"] = json!(warnings);
                }
                if let Some(w) = warnings
                    .iter()
                    .find(|w| SUSPICIOUS_WARNINGS.iter().any(|s| w.starts_with(s)))
                {
                    suspicious = Some(w.split(':').next().unwrap_or(w).to_string());
                } else if m.status_code == 200
                    && m.cache_state.as_deref() != Some("hit")
                    && m.content_type
                        .as_deref()
                        .is_some_and(|t| t.contains("html"))
                    && chars.is_some_and(|c| c < SHORT_TEXT_CHARS)
                {
                    suspicious = Some("short_text".into());
                }
            }
            Err(e) => {
                rec["errorCode"] = json!(e.code.as_str());
                rec["error"] = json!(truncate(&e.message, 300));
                if let Some(rule) = e
                    .details
                    .as_ref()
                    .and_then(|d| d.get("ruleId"))
                    .and_then(Value::as_str)
                {
                    rec["barrierRule"] = json!(rule);
                }
                // robots 禁止、URL 不合法、内网地址、不支持的参数是调用方的事，不算 pagesift 的可疑结果
                if !matches!(
                    e.code,
                    ErrorCode::RobotsDisallowed
                        | ErrorCode::InvalidRequest
                        | ErrorCode::SsrfBlocked
                        | ErrorCode::UnsupportedOption
                ) {
                    suspicious = Some("error".into());
                }
            }
        }
        if let Some(reason) = &suspicious {
            rec["suspicious"] = json!(reason);
            if let Some(html) = raw_html
                && let Some(path) = self.snapshot(url, html)
            {
                rec["snapshot"] = json!(path.display().to_string());
            }
        }
        self.write(&rec);
    }

    /// 调用方（MCP 的 `report_issue`）报告的问题，和抓取记录写进同一个日志。
    pub fn record_report(&self, report: Value) {
        let mut rec = self.base("report");
        if let Some(u) = report.get("url").and_then(Value::as_str) {
            rec["url"] = json!(self.clean_url(u));
        }
        for key in ["params", "expected", "actual", "note"] {
            if let Some(v) = report.get(key) {
                rec[key] = match v {
                    Value::String(s) => json!(truncate(s, 2000)),
                    other => other.clone(),
                };
            }
        }
        self.write(&rec);
    }

    fn base(&self, kind: &str) -> Value {
        let mut rec = json!({
            "ts": pagesift_api::rfc3339(SystemTime::now()),
            "kind": kind,
            "version": env!("CARGO_PKG_VERSION"),
            "source": self.cfg.source,
        });
        if let Ok(client) = CLIENT.try_with(Clone::clone) {
            rec["client"] = json!(client);
        }
        rec
    }

    fn write(&self, rec: &Value) {
        let mut line = rec.to_string();
        line.push('\n');
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        if log.len > 0
            && log.len + line.len() as u64 > self.cfg.max_bytes
            && let Err(e) = self.rotate(&mut log)
        {
            tracing::warn!("audit log rotation failed: {e}");
        }
        // 整行一次写入；文件以追加方式打开，每次写都落在文件末尾
        match log.file.write_all(line.as_bytes()) {
            Ok(()) => log.len += line.len() as u64,
            Err(e) => tracing::warn!("audit log write failed: {e}"),
        }
    }

    fn rotate(&self, log: &mut LogFile) -> io::Result<()> {
        let mut old = self.cfg.path.clone().into_os_string();
        old.push(".1");
        std::fs::rename(&self.cfg.path, &old)?;
        log.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cfg.path)?;
        log.len = 0;
        Ok(())
    }

    fn snapshot(&self, url: &str, html: &str) -> Option<PathBuf> {
        let dir = self.cfg.snapshot_dir.as_ref()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
        let day = now.as_secs() / 86_400;
        if self.snap_day.swap(day, Ordering::Relaxed) != day {
            self.snap_count.store(0, Ordering::Relaxed);
        }
        if self.snap_count.fetch_add(1, Ordering::Relaxed) >= self.cfg.snapshots_per_day {
            return None;
        }
        let date = pagesift_api::rfc3339(SystemTime::now());
        let dir = dir.join(date.get(..10).unwrap_or("unknown"));
        let hash = pagesift_content::hash_hex(url.as_bytes());
        let path = dir.join(format!(
            "{}-{}.html",
            now.as_millis(),
            hash.get(..12).unwrap_or(&hash)
        ));
        let mut end = html.len().min(self.cfg.snapshot_max_bytes);
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        // 快照可能有几 MB，不在异步线程上写
        let body = format!("<!-- pagesift snapshot of {url} -->\n{}", &html[..end]);
        let target = path.clone();
        let write = move || -> io::Result<()> {
            std::fs::create_dir_all(target.parent().unwrap_or(Path::new(".")))?;
            std::fs::write(&target, body)
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(move || {
                    if let Err(e) = write() {
                        tracing::warn!("audit snapshot write failed: {e}");
                    }
                });
            }
            Err(_) => write().ok()?,
        }
        Some(path)
    }

    fn clean_url(&self, raw: &str) -> String {
        if !self.cfg.redact {
            return raw.to_string();
        }
        redact_url(raw)
    }
}

/// 去掉 URL 里的用户名密码，查询参数里像凭证的值换成 `REDACTED`。
pub(crate) fn redact_url(raw: &str) -> String {
    let Ok(mut u) = Url::parse(raw) else {
        return raw.to_string();
    };
    let _ = u.set_username("");
    let _ = u.set_password(None);
    if u.query().is_some() {
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| {
                let v = if secret_param(&k) {
                    "REDACTED".to_string()
                } else {
                    v.into_owned()
                };
                (k.into_owned(), v)
            })
            .collect();
        u.query_pairs_mut().clear().extend_pairs(pairs);
    }
    u.to_string()
}

fn secret_param(name: &str) -> bool {
    const HINTS: &[&str] = &[
        "token",
        "key",
        "secret",
        "password",
        "passwd",
        "pwd",
        "auth",
        "session",
        "sig",
        "jwt",
        "credential",
        "code",
        "ticket",
    ];
    let n = name.to_ascii_lowercase();
    HINTS.iter().any(|h| n.contains(h))
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_credentials_but_keep_ordinary_parameters() {
        assert_eq!(
            redact_url("https://u:p@example.com/a?q=rust&api_key=abc&X-Amz-Signature=zz&page=2"),
            "https://example.com/a?q=rust&api_key=REDACTED&X-Amz-Signature=REDACTED&page=2"
        );
        assert_eq!(redact_url("not a url"), "not a url");
    }

    #[test]
    fn records_rotate_and_flag_suspicious_results() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = AuditConfig::new(dir.path().join("audit.jsonl"));
        cfg.max_bytes = 600;
        cfg.source = "test".into();
        let audit = Audit::open(cfg).unwrap();
        let mut doc = Document::default();
        doc.markdown = Some("tiny".into());
        doc.metadata.status_code = 200;
        doc.metadata.content_type = Some("text/html".into());
        let opts = ScrapeOptions::default();
        for _ in 0..4 {
            audit.record_scrape(
                "scrape",
                "https://example.com/p?token=t",
                &opts,
                &Ok(doc.clone()),
                Some("<html>x</html>"),
                Duration::from_millis(5),
            );
        }
        let log = std::fs::read_to_string(dir.path().join("audit.jsonl")).unwrap();
        let old = std::fs::read_to_string(dir.path().join("audit.jsonl.1")).unwrap();
        assert!(!log.is_empty() && !old.is_empty(), "rotated");
        let rec: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(rec["suspicious"], "short_text");
        assert_eq!(rec["url"], "https://example.com/p?token=REDACTED");
        assert_eq!(rec["textChars"], 4);
        let snap = rec["snapshot"].as_str().unwrap();
        assert!(std::path::Path::new(snap).exists());

        let err = ApiError::new(ErrorCode::RobotsDisallowed, "disallowed");
        audit.record_scrape(
            "scrape",
            "https://example.com/x",
            &opts,
            &Err(err),
            None,
            Duration::ZERO,
        );
        let log = std::fs::read_to_string(dir.path().join("audit.jsonl")).unwrap();
        let rec: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(rec["errorCode"], "ROBOTS_DISALLOWED");
        assert!(rec.get("suspicious").is_none());
    }
}
