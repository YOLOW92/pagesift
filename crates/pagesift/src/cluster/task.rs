//! 网关与工人之间的任务体和结果体。
//!
//! 两者都是 `[u64 LE 请求号][pagesift_sink::codec 编码的内容]`：请求号由网关在提交前登记，
//! 工人原样带回，网关据此把结果交给等待它的请求，不需要先拿到账本分配的任务编号
//! （提交和交付之间没有竞争窗口），也认不出的结果（上一次运行留下的、请求已取消的）直接丢弃。

use pagesift_api::types::{Document, ScrapeOptions};
use pagesift_api::{ApiError, ErrorCode};
use pagesift_cluster::SiteKey;
use pagesift_policy::RobotsMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const NONCE: usize = size_of::<u64>();

/// 一个页面任务（网关一侧借用，避免复制选项）。
#[derive(Serialize)]
struct PageTaskRef<'a> {
    kind: &'a str,
    url: &'a str,
    opts: &'a ScrapeOptions,
    /// robots 由网关判定（请求类型 + 管理员开关），工人照做。
    ignore_robots: bool,
    /// 服务端可信调用方租户（因 `ScrapeOptions.owner` 带有 `serde(skip)`）。
    owner: Option<&'a str>,
    client_session: Option<&'a str>,
}

/// 工人一侧解出的页面任务。
#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct PageTask {
    pub kind: String,
    pub url: String,
    pub opts: ScrapeOptions,
    pub ignore_robots: bool,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub client_session: Option<String>,
}

impl PageTask {
    pub fn mode(&self) -> RobotsMode {
        if self.ignore_robots {
            RobotsMode::Ignore
        } else {
            RobotsMode::Respect
        }
    }

    /// 审计记录里的请求类型：只认这两种，别的一律记成单页抓取。
    pub fn audit_kind(&self) -> &'static str {
        if self.kind == "crawl" {
            "crawl"
        } else {
            "scrape"
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Outcome {
    Page(Box<Document>),
    Failed(WireError),
}

/// 工人交回的结果：执行它的工人、它为这个任务写出的审计记录、页面或错误。
#[derive(Serialize, Deserialize)]
struct WireReply {
    worker: String,
    audit: Vec<Value>,
    outcome: Outcome,
}

/// 网关解出的结果。
pub(crate) struct Reply {
    pub worker: String,
    pub audit: Vec<Value>,
    pub result: Result<Document, ApiError>,
}

#[derive(Serialize, Deserialize)]
struct WireError {
    code: String,
    message: String,
    details: Option<Value>,
    retry_after: Option<u64>,
}

/// 全部错误码。新增错误码时要加进来，否则对端收到后按 `INTERNAL_ERROR` 报告（消息里保留原码）。
const CODES: &[ErrorCode] = &[
    ErrorCode::RobotsDisallowed,
    ErrorCode::AccessBarrier,
    ErrorCode::RateLimited,
    ErrorCode::HostCooldown,
    ErrorCode::RateLimitExceeded,
    ErrorCode::ConcurrencyLimitExceeded,
    ErrorCode::QuotaExceeded,
    ErrorCode::Timeout,
    ErrorCode::SsrfBlocked,
    ErrorCode::TooLarge,
    ErrorCode::UpstreamError,
    ErrorCode::UnsupportedOption,
    ErrorCode::InvalidRequest,
    ErrorCode::Unauthorized,
    ErrorCode::NotFound,
    ErrorCode::Internal,
    ErrorCode::DnsFailure,
    ErrorCode::NetworkError,
    ErrorCode::TlsCertExpired,
    ErrorCode::TlsHostnameMismatch,
    ErrorCode::TlsUntrustedRoot,
    ErrorCode::TlsError,
    ErrorCode::Busy,
];

fn framed(
    nonce: u64,
    body: impl FnOnce(&mut Vec<u8>) -> Result<(), pagesift_sink::codec::Error>,
) -> Vec<u8> {
    let mut out = nonce.to_le_bytes().to_vec();
    // 任务和结果都是本进程刚构造的普通数据结构，编码不会失败；失败说明类型里混进了不可编码的值
    body(&mut out).expect("page task and result types are always encodable");
    out
}

fn split(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let (head, rest) = bytes.split_first_chunk::<NONCE>()?;
    Some((u64::from_le_bytes(*head), rest))
}

pub(crate) fn encode_task(
    nonce: u64,
    kind: &str,
    url: &str,
    opts: &ScrapeOptions,
    mode: RobotsMode,
) -> Vec<u8> {
    let t = PageTaskRef {
        kind,
        url,
        opts,
        ignore_robots: mode == RobotsMode::Ignore,
        owner: opts.owner.as_deref(),
        client_session: opts.client_session.as_deref(),
    };
    framed(nonce, |out| pagesift_sink::codec::to_writer(out, &t))
}

pub(crate) fn decode_task(bytes: &[u8]) -> Result<(u64, PageTask), String> {
    let (nonce, rest) = split(bytes).ok_or("task body too short")?;
    let mut t: PageTask = pagesift_sink::codec::from_slice(rest).map_err(|e| e.to_string())?;
    t.opts.owner = t.owner.clone();
    t.opts.client_session = t.client_session.clone();
    Ok((nonce, t))
}

pub(crate) fn encode_outcome(
    nonce: u64,
    worker: &str,
    audit: Vec<Value>,
    r: &Result<Document, ApiError>,
) -> Vec<u8> {
    let outcome = match r {
        Ok(d) => Outcome::Page(Box::new(d.clone())),
        Err(e) => Outcome::Failed(WireError {
            code: e.code.as_str().to_owned(),
            message: e.message.clone(),
            details: e.details.clone(),
            retry_after: e.retry_after,
        }),
    };
    let reply = WireReply {
        worker: worker.to_owned(),
        audit,
        outcome,
    };
    framed(nonce, |out| pagesift_sink::codec::to_writer(out, &reply))
}

/// 只读请求号（交付时在账本的锁里调用，不解整个结果）。
pub(crate) fn nonce_of(bytes: &[u8]) -> Option<u64> {
    split(bytes).map(|(n, _)| n)
}

pub(crate) fn decode_outcome(bytes: &[u8]) -> Reply {
    let bad = |m: String| Reply {
        worker: String::new(),
        audit: Vec::new(),
        result: Err(ApiError::new(ErrorCode::Internal, m)),
    };
    let Some((_, rest)) = split(bytes) else {
        return bad("worker result too short".into());
    };
    let w: WireReply = match pagesift_sink::codec::from_slice(rest) {
        Ok(w) => w,
        Err(e) => return bad(format!("worker result: {e}")),
    };
    Reply {
        worker: w.worker,
        audit: w.audit,
        result: outcome(w.outcome),
    }
}

fn outcome(o: Outcome) -> Result<Document, ApiError> {
    match o {
        Outcome::Page(d) => Ok(*d),
        Outcome::Failed(w) => {
            let (code, message) = match CODES.iter().find(|c| c.as_str() == w.code) {
                Some(&c) => (c, w.message),
                None => (ErrorCode::Internal, format!("{}: {}", w.code, w.message)),
            };
            Err(ApiError {
                code,
                message,
                details: w.details,
                retry_after: w.retry_after,
            })
        }
    }
}

/// 站点键：注册域（与爬取的同站判断一致，`pagesift_content::site_key`）。
pub(crate) fn site_of(url: &str) -> SiteKey {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    SiteKey::of_domain(&pagesift_content::site_key(&host))
}

/// 任务类别（租约时长按类别统计）：请求的内核选择决定耗时的量级。
pub(crate) fn class_of(opts: &ScrapeOptions) -> u32 {
    match opts.engine.as_deref() {
        Some("http") => 1,
        Some("browser") => 2,
        _ => 3, // [协议] 本模块自定的类别编号（1 = HTTP，2 = 浏览器，3 = 自动），只用于区分耗时分布
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ScrapeOptions {
        serde_json::from_value(serde_json::json!({
            "formats": ["markdown", "links", {"type": "json", "prompt": "x"}],
            "headers": {"x-a": "1"},
            "onlyMainContent": false,
            "timeout": 1234,
            "engine": "http",
            "ignoreRobotsTxt": true
        }))
        .unwrap()
    }

    #[test]
    fn task_round_trips() {
        let mut o = opts();
        o.owner = Some("acct:alice".into());
        o.client_session = Some("sess-42".into());
        let b = encode_task(7, "crawl", "https://a.test/x", &o, RobotsMode::Ignore);
        let (n, t) = decode_task(&b).unwrap();
        assert_eq!(n, 7);
        assert_eq!(t.opts, o);
        assert_eq!(t.opts.owner.as_deref(), Some("acct:alice"));
        assert_eq!(t.opts.client_session.as_deref(), Some("sess-42"));
        assert_eq!(
            (t.kind.as_str(), t.url.as_str()),
            ("crawl", "https://a.test/x")
        );
        assert_eq!(t.mode(), RobotsMode::Ignore);
        assert_eq!(class_of(&o), 1);
    }

    #[test]
    fn outcome_round_trips() {
        let mut doc = Document {
            markdown: Some("# T\n\nbody".into()),
            links: Some(vec!["https://a.test/1".into()]),
            ..Default::default()
        };
        doc.metadata.title = Some("T".into());
        doc.metadata.url = Some("https://a.test/".into());
        let b = encode_outcome(
            9,
            "w1",
            vec![serde_json::json!({"kind": "scrape"})],
            &Ok(doc.clone()),
        );
        assert_eq!(nonce_of(&b), Some(9));
        let r = decode_outcome(&b);
        assert_eq!((r.worker.as_str(), r.audit.len()), ("w1", 1));
        assert_eq!(r.result.unwrap(), doc);
        for &c in CODES {
            let e = ApiError::new(c, "m")
                .with_details(serde_json::json!({"k": 1}))
                .with_retry_after(3);
            let r = decode_outcome(&encode_outcome(1, "w", Vec::new(), &Err(e.clone())));
            assert_eq!(r.result, Err(e));
        }
        assert!(decode_outcome(&[1, 2]).result.is_err());
    }

    #[test]
    fn sites_follow_the_registered_domain() {
        assert_eq!(
            site_of("https://a.example.com/x"),
            site_of("http://b.example.com/")
        );
        assert_ne!(
            site_of("https://a.example.com/"),
            site_of("https://example.org/")
        );
    }
}
