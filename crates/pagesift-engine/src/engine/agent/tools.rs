//! agent 的工具：search、map、带 queryFilter 的 scrape、extract。全都走 [`Engine`] 的正常入口，robots、
//! 按主机限速、SSRF 策略照常生效；模型只能决定网址和查询，抓取选项由这里定（不能关 robots）。

use pagesift_api::types::{
    AgentEvidence, AgentSource, Document, MapRequest, ScrapeOptions, SearchRequest,
};
use pagesift_api::{ApiError, ErrorCode, check_url};
use pagesift_extract::QueryFilterOptions;
use serde_json::{Value, json};
use url::Url;

use super::Run;
use super::protocol::clip;
use crate::engine::cpu::cpu_task;
use crate::focus::is_blind;

/// [策略] 模型没说要几条时取 5 条，和 Jina `/s/` 相同。
const DEFAULT_SEARCH_LIMIT: usize = 5;
/// [协议] `/v2/search` 的 `limit` 上限。
const MAX_SEARCH_LIMIT: usize = 100;

/// 一个还没读过的网址。
pub(super) struct Candidate {
    pub url: String,
    pub title: String,
    pub description: String,
    pub score: f32,
}

fn options(formats: Value) -> Result<ScrapeOptions, ApiError> {
    serde_json::from_value(json!({ "formats": formats }))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

impl Run<'_> {
    /// 这个网址能不能打开：协议合法、`strictConstrainToURLs`、`allowedHosts`。
    pub(super) fn url_allowed(&self, url: &str) -> Result<(), String> {
        check_url(url).map_err(|e| e.message)?;
        if self.req.strict_constrain_to_urls {
            let wanted = url.trim_end_matches('/');
            let listed = self
                .req
                .urls
                .iter()
                .flatten()
                .any(|u| u.trim_end_matches('/') == wanted);
            if !listed {
                return Err("strictConstrainToURLs: this URL is not in the urls list".into());
            }
        }
        if let Some(hosts) = &self.req.allowed_hosts {
            let parsed = Url::parse(url).map_err(|e| e.to_string())?;
            let host = parsed.host_str().unwrap_or_default();
            let ok = hosts
                .iter()
                .any(|h| host == h || host.ends_with(&format!(".{h}")));
            if !ok {
                return Err(format!("allowedHosts: {host} is not allowed"));
            }
        }
        Ok(())
    }

    fn add_candidate(&mut self, url: &str, title: &str, description: &str) {
        if self.read.contains(url) || self.url_allowed(url).is_err() {
            return;
        }
        let Ok(parsed) = Url::parse(url) else { return };
        let score = self
            .focus
            .link_score(&parsed, Some(title), Some(description), 0.0, 1);
        match self.candidates.iter_mut().find(|c| c.url == url) {
            Some(c) => c.score = c.score.max(score),
            None => self.candidates.push(Candidate {
                url: url.to_string(),
                title: title.to_string(),
                description: description.to_string(),
                score,
            }),
        }
    }

    /// 把请求里的 `urls` 放进候选：它们不算盲探。
    pub(super) fn seed_candidates(&mut self) {
        for u in self.req.urls.iter().flatten() {
            self.seeds.insert(u.clone());
            self.add_candidate(u, "", "given by the caller");
        }
    }

    pub(super) async fn tool_search(&mut self, query: &str, limit: Option<usize>) {
        let limit = limit
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT);
        let input = json!({ "query": query, "limit": limit });
        let req: Result<SearchRequest, _> =
            serde_json::from_value(json!({ "query": query, "limit": limit }));
        let found = match req {
            Ok(r) => self.engine.search(&r).await,
            Err(e) => Err(ApiError::invalid(e.to_string())),
        };
        let web = match found {
            Ok(v) => v["web"].as_array().cloned().unwrap_or_default(),
            Err(e) => {
                self.record("search", input, e.code.as_str(), Some(e.message.clone()))
                    .await;
                self.last = format!("search failed: {}", e.message);
                return;
            }
        };
        self.usage.searches += 1;
        let mut lines = Vec::new();
        for item in &web {
            let (Some(url), title, desc) = (
                item["url"].as_str(),
                item["title"].as_str().unwrap_or_default(),
                item["description"].as_str().unwrap_or_default(),
            ) else {
                continue;
            };
            self.add_candidate(url, title, desc);
            if !desc.is_empty() && self.url_allowed(url).is_ok() {
                // 摘要也是证据：回答可能直接来自它，要能查到是哪个网址说的
                let id = self.push_evidence(url, "search", Some(query), desc.to_string(), None);
                self.add_source(url, Some(title), &id, None);
            }
            lines.push(url.to_string());
        }
        self.last = format!(
            "search {query:?} returned {} results (see the candidate list)",
            lines.len()
        );
        let detail = format!("{} results", lines.len());
        self.record("search", input, "ok", Some(detail)).await;
    }

    pub(super) async fn tool_map(&mut self, url: &str, search: Option<&str>) {
        let input = json!({ "url": url, "search": search });
        if let Err(why) = self.url_allowed(url) {
            self.last = format!("map {url} refused: {why}");
            self.record("map", input, "REFUSED", Some(why)).await;
            return;
        }
        let req: Result<MapRequest, _> =
            serde_json::from_value(json!({ "url": url, "search": search }));
        let found = match req {
            Ok(r) => self.engine.map(&r).await,
            Err(e) => Err(ApiError::invalid(e.to_string())),
        };
        match found {
            Ok(links) => {
                self.usage.maps += 1;
                let n = links.len();
                for l in links {
                    self.add_candidate(
                        &l.url,
                        l.title.as_deref().unwrap_or_default(),
                        l.description.as_deref().unwrap_or_default(),
                    );
                }
                self.last = format!("map {url} found {n} URLs (see the candidate list)");
                self.record("map", input, "ok", Some(format!("{n} URLs")))
                    .await;
            }
            Err(e) => {
                self.note_failure("map", url, input, &e).await;
            }
        }
    }

    /// 出错的网址：robots 拒绝单独写进 warnings，其余的告诉模型换一个。
    async fn note_failure(&mut self, tool: &str, url: &str, input: Value, e: &ApiError) {
        if e.code == ErrorCode::RobotsDisallowed {
            self.warnings
                .push(format!("agent_robots_disallowed: {url}"));
            self.last.push_str(&format!(
                "{url} is disallowed by robots.txt; do not retry it.\n"
            ));
        } else {
            self.last.push_str(&format!(
                "{url} failed: {}: {}\n",
                e.code.as_str(),
                e.message
            ));
        }
        self.record(tool, input, e.code.as_str(), Some(e.message.clone()))
            .await;
    }

    /// 还能抓几页（页数上限、额度上限、调用方配额中最小的）。
    pub(super) fn pages_room(&self) -> u64 {
        let mut room = self.budget.max_pages.saturating_sub(self.usage.pages);
        if let Some(c) = self.budget.max_credits {
            room = room.min(c.saturating_sub(self.usage.credits()));
        }
        if let Some(q) = self.sink.pages_left() {
            room = room.min(q);
        }
        room
    }

    pub(super) async fn tool_scrape(&mut self, urls: Vec<String>, query: Option<String>) {
        let query = query.unwrap_or_else(|| self.req.prompt.clone());
        self.last.clear();
        let mut todo: Vec<String> = Vec::new();
        for u in urls {
            if todo.contains(&u) {
                continue;
            }
            if self.read.contains(&u) {
                self.last
                    .push_str(&format!("{u} was already read; use your notes.\n"));
                continue;
            }
            if let Err(why) = self.url_allowed(&u) {
                self.last.push_str(&format!("{u} refused: {why}\n"));
                self.record("scrape", json!({ "url": u }), "REFUSED", Some(why))
                    .await;
                continue;
            }
            todo.push(u);
        }
        let room = self.pages_room() as usize;
        if todo.len() > room {
            let skipped = todo.split_off(room);
            self.last.push_str(&format!(
                "budget: these URLs were not read: {}\n",
                skipped.join(", ")
            ));
        }
        let opts = match options(json!(["markdown", "links"])) {
            Ok(o) => o,
            Err(e) => {
                self.last.push_str(&e.message);
                return;
            }
        };
        let engine = self.engine;
        let opts = &opts;
        let results = futures_util::future::join_all(
            todo.iter()
                .map(|u| async move { (u.as_str(), engine.scrape(u, opts).await) }),
        )
        .await;
        self.fresh.clear();
        for (url, r) in results {
            self.read.insert(url.to_string());
            let input = json!({ "url": url, "query": query });
            match r {
                Ok(doc) => {
                    self.usage.pages += 1;
                    self.sink.charge_page().await;
                    self.page(url, &query, input, doc).await;
                }
                Err(e) => {
                    self.usage.pages_failed += 1;
                    self.note_failure("scrape", url, input, &e).await;
                }
            }
        }
        let read = &self.read;
        self.candidates.retain(|c| !read.contains(&c.url));
    }

    /// 一页抓回来：更新停止判据（整页的相关度），存档 queryFilter 留下的部分。
    async fn page(&mut self, url: &str, query: &str, input: Value, doc: Document) {
        let markdown = doc.markdown.clone().unwrap_or_default();
        let parsed = Url::parse(url).ok();
        let title = doc.metadata.title.clone();
        let links = doc.links.as_ref().map_or(0, Vec::len);
        let mut relevance = 0.0;
        if let Some(u) = &parsed {
            let signal = self.focus.page_signal(
                u,
                title.as_deref(),
                &markdown,
                links,
                doc.metadata.language.as_deref(),
            );
            self.focus.observe_page(&signal);
            relevance = self.focus.relevance(&signal);
            // 有线索才不算盲探：调用方给的网址、标题摘要里有查询词的候选
            let score = self
                .candidates
                .iter()
                .find(|c| c.url == url)
                .map_or(0.0, |c| c.score);
            let blind = !self.seeds.contains(url) && is_blind(score);
            if self.verdict.is_none() {
                self.verdict =
                    self.adaptive
                        .observe(&self.focus.query, &markdown, relevance, blind);
            }
        }

        let cap = self.budget.page_tokens;
        let defaults = QueryFilterOptions::default();
        let opts = QueryFilterOptions {
            max_tokens: Some(cap),
            min_tokens: defaults.min_tokens.map(|m| m.min(cap)),
            ..defaults
        };
        let q = query.to_string();
        let md = markdown;
        let kept = cpu_task(move || pagesift_extract::filter_markdown_by_query(&md, &q, &opts))
            .await
            .map(|f| f.markdown)
            .unwrap_or_default();
        let text = clip(&kept, cap).to_string();
        if text.trim().is_empty() {
            self.last
                .push_str(&format!("{url} has no text matching the query.\n"));
            self.record("scrape", input, "ok", Some("no matching text".into()))
                .await;
            return;
        }
        let hash = doc
            .metadata
            .pagesift
            .as_ref()
            .and_then(|m| m.content_hash.clone());
        let id = self.push_evidence(url, "page", Some(query), text, Some(relevance));
        self.add_source(url, title.as_deref(), &id, hash);
        self.fresh.push(self.evidence.len() - 1);
        self.record("scrape", input, "ok", Some(format!("evidence {id}")))
            .await;
    }

    pub(super) async fn tool_extract(&mut self, url: &str, intent: &str) {
        let input = json!({ "url": url, "intent": intent });
        self.last.clear();
        self.fresh.clear();
        if self.read.contains(url)
            && self
                .evidence
                .iter()
                .any(|e| e.url == url && e.kind == "extract")
        {
            self.last = format!("{url} was already extracted; use your notes.");
            return;
        }
        if let Err(why) = self.url_allowed(url) {
            self.last = format!("{url} refused: {why}");
            self.record("extract", input, "REFUSED", Some(why)).await;
            return;
        }
        if self.pages_room() == 0 {
            self.last = format!("budget: {url} was not read");
            return;
        }
        let opts = match options(json!([{ "type": "extract", "intent": intent }])) {
            Ok(o) => o,
            Err(e) => {
                self.last = e.message;
                return;
            }
        };
        match self.engine.scrape(url, &opts).await {
            Ok(doc) => {
                self.usage.pages += 1;
                self.read.insert(url.to_string());
                self.sink.charge_page().await;
                let notes = doc.metadata.pagesift.as_ref().map(|m| m.warnings.clone());
                self.count_tool_llm_use(notes.as_deref().unwrap_or_default());
                let items = doc.extract.clone().unwrap_or(Value::Null);
                let text = clip(&items.to_string(), self.budget.page_tokens).to_string();
                let hash = doc
                    .metadata
                    .pagesift
                    .as_ref()
                    .and_then(|m| m.content_hash.clone());
                let id = self.push_evidence(url, "extract", Some(intent), text, None);
                self.add_source(url, doc.metadata.title.as_deref(), &id, hash);
                self.fresh.push(self.evidence.len() - 1);
                self.record("extract", input, "ok", Some(format!("evidence {id}")))
                    .await;
                let read = &self.read;
                self.candidates.retain(|c| !read.contains(&c.url));
            }
            Err(e) => {
                self.usage.pages_failed += 1;
                self.note_failure("extract", url, input, &e).await;
            }
        }
    }

    /// `extract` 的 schema 生成也调模型：结果的 `llm_usage: N calls, M tokens` 计入本次运行。
    fn count_tool_llm_use(&mut self, warnings: &[String]) {
        for w in warnings {
            let Some(rest) = w.strip_prefix("llm_usage: ") else {
                continue;
            };
            let mut nums = rest
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<u64>().ok());
            self.usage.llm_calls += nums.next().unwrap_or(0);
            self.usage.tokens += nums.next().unwrap_or(0);
            self.usage.tokens_estimated = true;
        }
    }

    pub(super) fn push_evidence(
        &mut self,
        url: &str,
        kind: &str,
        query: Option<&str>,
        text: String,
        relevance: Option<f32>,
    ) -> String {
        let id = format!("E{}", self.evidence.len() + 1);
        self.evidence.push(AgentEvidence {
            id: id.clone(),
            url: url.to_string(),
            kind: kind.to_string(),
            query: query.map(str::to_string),
            text,
            relevance,
        });
        id
    }

    pub(super) fn add_source(
        &mut self,
        url: &str,
        title: Option<&str>,
        evidence: &str,
        hash: Option<String>,
    ) {
        let now = pagesift_api::rfc3339(std::time::SystemTime::now());
        match self.sources.iter_mut().find(|s| s.url == url) {
            Some(s) => {
                s.evidence_ids.push(evidence.to_string());
                if s.title.as_deref().is_none_or(str::is_empty) {
                    s.title = title.map(str::to_string);
                }
                if hash.is_some() {
                    s.content_hash = hash;
                    s.fetched_at = Some(now);
                }
            }
            None => self.sources.push(AgentSource {
                url: url.to_string(),
                title: title.map(str::to_string),
                evidence_ids: vec![evidence.to_string()],
                fetched_at: hash.is_some().then_some(now),
                content_hash: hash,
            }),
        }
    }
}
