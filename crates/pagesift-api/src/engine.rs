//! 抓取引擎接口。API 层只做协议、校验和任务管理，真正的抓取由实现这个 trait 的引擎完成。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::ApiError;
use crate::types::{
    AgentFailure, AgentOutcome, AgentRequest, AgentStep, CrawlRequest, CrawlSummary, Document,
    ExtractRequest, InteractRequest, InteractResponse, InteractStopped, MapLink, MapRequest,
    ScrapeOptions, SearchRequest, Upload,
};
use serde_json::Value;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 每次抓取前取得页面预算；拒绝时不得发出页面请求。
pub trait PageBudget: Send + Sync {
    fn acquire(&self) -> BoxFuture<'_, Result<(), ApiError>>;
}

pub trait Engine: Send + Sync + 'static {
    fn scrape<'a>(
        &'a self,
        url: &'a str,
        opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>>;

    /// `/v2/parse`：解析调用方上传的文件（PDF、HTML），返回和 scrape 相同的文档。
    /// `opts` 已经过 [`crate::check_parse_options`] 校验。
    fn parse<'a>(
        &'a self,
        _file: Upload,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "parse is not available in this build",
            ))
        })
    }

    fn map<'a>(&'a self, _req: &'a MapRequest) -> BoxFuture<'a, Result<Vec<MapLink>, ApiError>> {
        Box::pin(async { Err(ApiError::unsupported("map is not available in this build")) })
    }

    /// 整站爬取：抓到一页、出错一页、发现新 URL 时都通过 `sink` 回报，直到爬完或被取消。
    fn crawl<'a>(
        &'a self,
        _req: &'a CrawlRequest,
        _sink: Arc<dyn CrawlSink>,
    ) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "crawl is not available in this build",
            ))
        })
    }

    /// `/v2/search`：返回 Firecrawl 的 `data`（`{ "web": [...] }`）。
    fn search<'a>(&'a self, _req: &'a SearchRequest) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "search is not configured: set [search] in pagesift.toml",
            ))
        })
    }

    /// `/v2/extract`：返回提取出的 JSON。
    fn extract<'a>(&'a self, _req: &'a ExtractRequest) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "extract needs an LLM: set [llm] in pagesift.toml",
            ))
        })
    }

    fn search_budgeted<'a>(
        &'a self,
        _req: &'a SearchRequest,
        _budget: Arc<dyn PageBudget>,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "page quotas are not supported by this search engine",
            ))
        })
    }

    fn extract_budgeted<'a>(
        &'a self,
        _req: &'a ExtractRequest,
        _budget: Arc<dyn PageBudget>,
    ) -> BoxFuture<'a, Result<Value, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "page quotas are not supported by this extract engine",
            ))
        })
    }

    /// `/v2/agent` 开跑前的检查：没配 LLM、没有 `urls` 又没配搜索时，提交时就报错。
    fn agent_check(&self, _req: &AgentRequest) -> Result<(), ApiError> {
        Err(ApiError::unsupported(
            "agent needs an LLM: set [llm] in pagesift.toml",
        ))
    }

    /// `/v2/agent`：跑完一次（中途出错也回已经攒下的证据，错误在 `AgentOutcome::error`）。
    fn agent<'a>(
        &'a self,
        _req: &'a AgentRequest,
        _sink: Arc<dyn AgentSink>,
    ) -> BoxFuture<'a, AgentOutcome> {
        Box::pin(async {
            AgentOutcome {
                error: Some(AgentFailure::from(&ApiError::unsupported(
                    "agent needs an LLM: set [llm] in pagesift.toml",
                ))),
                ..Default::default()
            }
        })
    }

    /// 引擎支持的 `formats`；请求了不支持的格式时返回 UNSUPPORTED_OPTION，不悄悄忽略。
    fn supported_formats(&self) -> &'static [&'static str] {
        &["markdown", "html", "rawHtml", "links", "images"]
    }

    /// 是否支持 `actions`（需要浏览器渲染）。
    fn supports_actions(&self) -> bool {
        false
    }

    /// 不支持 `actions` 时给调用方的说明。
    fn actions_unavailable(&self) -> &'static str {
        "actions need browser rendering, which is not available in this build"
    }

    /// 是否支持 `/v2/scrape/{id}/interact`。支持时 `/v2/scrape` 走 [`Engine::scrape_session`]。
    fn supports_interact(&self) -> bool {
        false
    }

    /// `/v2/scrape`：抓一页，结果带 `metadata.scrapeId`。`keep` 时保留浏览器页面（`keepSession`），
    /// 否则只记下回放信息。`owner` 是调用方的账号，之后的 interact 只认同一个账号。
    fn scrape_session<'a>(
        &'a self,
        _url: &'a str,
        _opts: &'a ScrapeOptions,
        _keep: bool,
        _owner: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Document, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "keepSession is not available in this build",
            ))
        })
    }

    /// `/v2/observe`：页面上和指令对应的候选元素（选择器、角色、名称），返回候选和用量警告。
    fn observe<'a>(
        &'a self,
        _url: &'a str,
        _instruction: &'a str,
    ) -> BoxFuture<'a, Result<crate::types::Observed, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "observe is not available in this build",
            ))
        })
    }

    /// `/v2/similar`：和例子结构相同的兄弟元素。
    fn find_similar<'a>(
        &'a self,
        _url: &'a str,
        _selector: &'a str,
        _opts: &'a ScrapeOptions,
    ) -> BoxFuture<'a, Result<crate::types::SimilarData, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "find_similar is not available in this build",
            ))
        })
    }

    fn interact<'a>(
        &'a self,
        _id: &'a str,
        _req: &'a InteractRequest,
        _owner: Option<&'a str>,
    ) -> BoxFuture<'a, Result<InteractResponse, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "interact is not available in this build",
            ))
        })
    }

    fn stop_interact<'a>(
        &'a self,
        _id: &'a str,
        _owner: Option<&'a str>,
    ) -> BoxFuture<'a, Result<InteractStopped, ApiError>> {
        Box::pin(async {
            Err(ApiError::unsupported(
                "interact is not available in this build",
            ))
        })
    }

    /// 统一准入现在允许的在途抓取数：批量任务的并发按它定，用户的
    /// `max_batch_concurrency` 只作上限。引擎还没接准入时返回 `None`，API 用配置或先验。
    fn admitted_concurrency(&self) -> Option<usize> {
        None
    }
}

/// agent 运行的回报和额度，由 API 层的任务管理实现；`()` 是不回报、不限额的实现。
pub trait AgentSink: Send + Sync {
    /// 一次工具调用结束；`credits` 是到现在为止用掉的额度。
    fn step<'a>(&'a self, _step: &'a AgentStep, _credits: u64) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    /// 调用方要求取消：在两步之间生效。
    fn cancelled(&self) -> bool {
        false
    }
    /// 发出一次页面抓取之前调用：原子预留这一页的配额；拒绝时不得发请求。
    fn reserve_page<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async { Ok(()) })
    }
    /// 这一页抓回并会交付：结算预留。
    fn charge_page<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    /// 这一页失败或没有交付：退还预留。
    fn release_page<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    /// 调用方今天还能抓多少页；没有配额时 `None`。
    fn pages_left(&self) -> Option<u64> {
        None
    }
}

impl AgentSink for () {}

/// 整站爬取的进度回报，由 API 层的任务管理实现。
pub trait CrawlSink: Send + Sync {
    fn page(&self, doc: Document) -> BoxFuture<'_, ()>;
    fn error<'a>(&'a self, url: &'a str, err: ApiError) -> BoxFuture<'a, ()>;

    /// 发出一次页面抓取之前调用：原子预留这一页的配额。返回 `Err` 时引擎必须停止调度、
    /// 不得再发请求（重试同一 URL 会再次调用，实现要按 URL 幂等）。
    fn reserve_page<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, Result<(), ApiError>> {
        Box::pin(async { Ok(()) })
    }

    /// 这一页已经成功抓回并会交付：结算预留。之后引擎才调用 [`CrawlSink::page`]。
    fn settle_page<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// 这一页最终失败（不再重试）：退还预留。
    fn release_page<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// 本次爬取结束：退还未结算的预留（取消、上限、客户端断开都走这里）。
    fn release_pending(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    /// 已经确定要抓的页面总数（随着发现新链接增长）。
    fn total(&self, total: usize) -> BoxFuture<'_, ()>;
    fn cancelled(&self) -> BoxFuture<'_, bool>;

    /// 断点续爬：任务是从中断处恢复的时候返回上次的进度，引擎据此重建队列和去重表，
    /// 不再抓已经完成的页面。新任务返回 `None`。
    fn resume(&self) -> Option<CrawlResume> {
        None
    }

    /// 是否需要 [`CrawlSink::enqueued`] 回报（持久化的任务需要）。不需要时引擎不必收集。
    fn tracks_frontier(&self) -> bool {
        false
    }

    /// 新进入队列的 URL 和发现深度，按入队顺序。只在 [`CrawlSink::tracks_frontier`] 为真时调用。
    fn enqueued(&self, _urls: Vec<(String, usize)>) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// 爬取结束时的说明（停止原因等）；只有带查询的爬取会调用。
    fn summary(&self, _summary: CrawlSummary) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Emit one page's streaming DataGraph delta during a graph-enabled crawl.
    fn graph_delta(&self, _delta: pagesift_graph::PageGraphDelta) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Emit the reconciled job-level DataGraph summary when a graph-enabled crawl finishes.
    fn graph_summary(&self, _summary: pagesift_graph::GraphSummary) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// 中断的爬取任务恢复时的进度。完成的页面以文档的 `metadata.sourceURL`（失败的以出错的 URL）为准。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrawlResume {
    /// 入过队的全部 URL，按入队顺序（去重表）。
    pub enqueued: Vec<String>,
    /// 入了队但还没完成的 URL 和发现深度，按入队顺序。中断时正在抓的页面也在这里。
    pub pending: Vec<(String, usize)>,
    /// 已经完成的页数（成功和失败都算），计入 `limit`。
    pub done: usize,
}
