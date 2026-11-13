//! `/v2/agent` 的循环：LLM 规划 → 调工具（search、map、带 queryFilter 的 scrape、extract）→ 证据存档 →
//! 判断够不够 → 停。
//!
//! - **停**：模型自己说够了（`answered`），或者自适应爬取的停止判据（[`Adaptive`]：新页面不再带来相关信息）
//!   先触发，这时只允许模型回答，不能再调工具。`effort` 只改预算上限，判据不变。
//! - **预算用尽**：同样只允许回答。回答有缺失、不符合 schema 或有未核实的字段时如实报告不完整
//!   （`budget.exhausted`，对外 `failed` + `stopReason`）；没有 token 回答时只留下证据。
//! - **token**：每次提示的上限来自 `model_limits()` 和 `token_rate_limit()`（见 [`budget`]），超过每分钟
//!   限额时等。用量优先取服务报的值，没报时按 [`model_tokens`] 估计并标出来。

mod budget;
mod protocol;
mod provenance;
mod tools;

use std::collections::HashSet;
use std::time::Instant;

use pagesift_api::types::{
    AdaptiveOptions, AgentEvidence, AgentFailure, AgentOutcome, AgentRequest, AgentSource,
    AgentStep, AgentUsage,
};
use pagesift_api::{AgentSink, ApiError, Engine, ErrorCode};
use pagesift_llm::{
    ChatClient, ChatMessage, ChatRequest, LlmError, ResponseFormat, Role, SchemaMode,
};
use serde_json::Value;

use self::budget::{Budget, Pacer};
use self::protocol::{Action, Tools, View, parse_action, system_prompt, user_prompt};
use self::tools::Candidate;
use super::autoschema::model_tokens;
use super::extract::llm_error;
use crate::focus::{Adaptive, Focus};

/// 提交前的检查：没配 LLM、没有 `urls` 又没配搜索。
pub fn check(has_llm: bool, has_search: bool, req: &AgentRequest) -> Result<(), ApiError> {
    if !has_llm {
        return Err(ApiError::unsupported(
            "agent needs an LLM: set [llm] in pagesift.toml",
        ));
    }
    if !has_search && req.urls.as_deref().unwrap_or_default().is_empty() {
        return Err(ApiError::unsupported(
            "agent without urls needs a search backend: set [search] in pagesift.toml",
        ));
    }
    Ok(())
}

/// 跑完一次。`engine` 可以是单机的 `Scraper`，也可以是多服务器网关（页面走网关的调度）。
pub async fn run(
    engine: &dyn Engine,
    llm: &ChatClient,
    has_search: bool,
    req: &AgentRequest,
    sink: &dyn AgentSink,
) -> AgentOutcome {
    let budget = Budget::new(llm, req).await;
    let pacer = Pacer::new(budget.per_minute);
    let focus = Focus::new(&req.prompt);
    let terms = focus.query.len();
    let strict = req.strict_constrain_to_urls;
    let mut run = Run {
        engine,
        llm,
        req,
        sink,
        tools: Tools {
            search: has_search && !strict,
            map: !strict,
        },
        budget,
        pacer,
        adaptive: Adaptive::new(AdaptiveOptions::default(), terms),
        focus,
        verdict: None,
        candidates: Vec::new(),
        seeds: HashSet::new(),
        known: HashSet::new(),
        untrusted: false,
        read: HashSet::new(),
        evidence: Vec::new(),
        fresh: Vec::new(),
        sources: Vec::new(),
        steps: Vec::new(),
        step_lines: Vec::new(),
        notes: String::new(),
        last: String::new(),
        usage: AgentUsage::default(),
        warnings: Vec::new(),
        turn: 0,
        native_json: llm.config().schema_mode != SchemaMode::PromptOnly,
    };
    run.seed_candidates();
    run.drive().await
}

/// 为什么只能回答了。
#[derive(Clone, Copy)]
enum Stop {
    /// 自适应判据：新页面不再带来相关信息。
    Adaptive(&'static str),
    Budget(&'static str),
}

struct Run<'a> {
    engine: &'a dyn Engine,
    llm: &'a ChatClient,
    req: &'a AgentRequest,
    sink: &'a dyn AgentSink,
    tools: Tools,
    budget: Budget,
    pacer: Pacer,
    focus: Focus,
    adaptive: Adaptive,
    verdict: Option<&'static str>,
    candidates: Vec<Candidate>,
    /// 调用方给的网址：不算盲探。
    seeds: HashSet<String>,
    /// 来路清楚的网址：调用方给的、搜索和 map 返回的、读过的页面上链接的。
    known: HashSet<String>,
    /// 模型已经读到网页内容（可能带注入的指令）：之后只能打开 `known` 里的网址，
    /// 否则页面可以让它去抓 `https://攻击者/?q=<任务和笔记>`。
    untrusted: bool,
    /// 抓过的网址（成功、失败、被 robots 拒绝都算），不再抓第二次。
    read: HashSet<String>,
    evidence: Vec<AgentEvidence>,
    /// 上一步新增的证据（下标）。
    fresh: Vec<usize>,
    sources: Vec<AgentSource>,
    steps: Vec<AgentStep>,
    step_lines: Vec<String>,
    notes: String,
    last: String,
    usage: AgentUsage,
    warnings: Vec<String>,
    turn: usize,
    /// 服务接受 `response_format`；拒绝过一次（400 等）后改用提示约束。
    native_json: bool,
}

impl Run<'_> {
    async fn record(&mut self, tool: &str, input: Value, result: &str, detail: Option<String>) {
        let step = AgentStep {
            turn: self.turn,
            tool: tool.to_string(),
            input,
            result: result.to_string(),
            detail,
        };
        self.step_lines.push(format!(
            "turn {}: {} {} -> {}{}",
            step.turn,
            step.tool,
            step.input,
            step.result,
            step.detail
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        ));
        self.sink.step(&step, self.usage.credits()).await;
        self.steps.push(step);
    }

    /// 接下来只能回答了吗。预算里要留出回答和一次纠正。
    fn forced_stop(&self) -> Option<Stop> {
        if let Some(v) = self.verdict {
            return Some(Stop::Adaptive(v));
        }
        if self.usage.pages >= self.budget.max_pages {
            return Some(Stop::Budget("pages"));
        }
        if let Some(c) = self.budget.max_credits
            && self.usage.credits() >= c
        {
            return Some(Stop::Budget("credits"));
        }
        if self.sink.pages_left() == Some(0) {
            return Some(Stop::Budget("quota"));
        }
        if self.usage.llm_calls + 2 >= self.budget.max_calls {
            return Some(Stop::Budget("llmCalls"));
        }
        if self.usage.tokens + 2 * self.call_cost() as u64 > self.budget.max_tokens {
            return Some(Stop::Budget("tokens"));
        }
        None
    }

    /// 一次调用最多用多少 token。
    fn call_cost(&self) -> usize {
        self.budget.input + self.budget.output
    }

    fn budget_line(&self) -> String {
        let mut line = format!(
            "{} of {} pages read, {} of {} model calls used",
            self.usage.pages, self.budget.max_pages, self.usage.llm_calls, self.budget.max_calls
        );
        if let Some(c) = self.budget.max_credits {
            line.push_str(&format!(", {} of {c} credits used", self.usage.credits()));
        }
        line
    }

    /// 组装这一轮的提示。任务本身放不下时返回 `None`。
    fn prompt(&mut self, stop: Option<Stop>, correction: Option<&str>) -> Option<(String, String)> {
        let system = system_prompt(self.tools);
        self.candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        let final_turn = stop.map(|s| match s {
            Stop::Adaptive(_) => "new pages stopped adding relevant information",
            Stop::Budget(_) => "the budget is used up",
        });
        let view = View {
            task: &self.req.prompt,
            schema: self.req.schema.as_ref(),
            budget_line: self.budget_line(),
            steps: &self.step_lines,
            notes: &self.notes,
            candidates: self
                .candidates
                .iter()
                .map(|c| (c.url.as_str(), c.title.as_str(), c.description.as_str()))
                .collect(),
            fresh: &self.fresh,
            evidence: &self.evidence,
            last: &self.last,
            correction,
            final_turn,
            input_tokens: self.budget.input,
        };
        let user = user_prompt(&view, model_tokens(&system))?;
        Some((system, user))
    }

    /// 调一次模型（等每分钟限额），记下用量。
    async fn ask(&mut self, system: String, user: String) -> Result<String, LlmError> {
        let estimate = model_tokens(&system) + model_tokens(&user);
        let wait = self.pacer.wait_for(estimate, Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let mut req = ChatRequest {
            messages: vec![
                ChatMessage::new(Role::System, system),
                ChatMessage::new(Role::User, user),
            ],
            temperature: Some(0.0),
            max_tokens: Some(self.budget.output.min(u32::MAX as usize) as u32),
            response_format: self.native_json.then_some(ResponseFormat::JsonObject),
        };
        self.usage.llm_calls += 1;
        let resp = match self.llm.chat(&req).await {
            // [协议] 服务拒绝 response_format 的状态码，和 `extract_json` 相同
            Err(LlmError::Upstream { status, .. })
                if self.native_json
                    && self.llm.config().schema_mode == SchemaMode::Auto
                    && matches!(status, 400 | 404 | 415 | 422) =>
            {
                // 服务不认 response_format，或者回复过不了它的 JSON 校验：退回提示约束
                self.native_json = false;
                req.response_format = None;
                self.usage.llm_calls += 1;
                self.llm.chat(&req).await?
            }
            other => other?,
        };
        let spent = match resp.usage {
            Some(u) => u.total,
            None => {
                self.usage.tokens_estimated = true;
                estimate + model_tokens(&resp.content)
            }
        };
        self.usage.tokens += spent as u64;
        self.pacer.record(spent, Instant::now());
        Ok(resp.content)
    }

    fn outcome(&mut self, finish_reason: &str) -> AgentOutcome {
        AgentOutcome {
            finish_reason: finish_reason.to_string(),
            sources: std::mem::take(&mut self.sources),
            evidence: std::mem::take(&mut self.evidence),
            steps: std::mem::take(&mut self.steps),
            usage: self.usage.clone(),
            budget: self.budget.report(None),
            warnings: std::mem::take(&mut self.warnings),
            ..Default::default()
        }
    }

    fn failed(&mut self, error: ApiError) -> AgentOutcome {
        let mut o = self.outcome("error");
        o.error = Some(AgentFailure::from(&error));
        o
    }

    /// 没有 token 或调用次数回答了：只留下证据。
    fn out_of_budget(&mut self, kind: &str) -> AgentOutcome {
        let mut o = self.outcome("budget_exhausted");
        o.budget.exhausted = Some(kind.to_string());
        o.message = Some(format!(
            "the {kind} budget ran out before the model could answer; only the evidence is available"
        ));
        o
    }

    async fn drive(&mut self) -> AgentOutcome {
        let mut correction: Option<String> = None;
        loop {
            if self.sink.cancelled() {
                return self.outcome("cancelled");
            }
            let stop = self.forced_stop();
            // 回答也要花一次调用：一次都花不起时不再调用
            if let Some(Stop::Budget(kind)) = stop
                && (self.usage.llm_calls >= self.budget.max_calls
                    || self.usage.tokens + self.call_cost() as u64 > self.budget.max_tokens)
            {
                return self.out_of_budget(kind);
            }
            let Some((system, user)) = self.prompt(stop, correction.as_deref()) else {
                return self.failed(ApiError::invalid(format!(
                    "the prompt and schema do not fit the {} tokens this model allows per call",
                    self.budget.input
                )));
            };
            self.turn += 1;
            let reply = match self.ask(system, user).await {
                Ok(r) => r,
                // 回复的格式不对（空内容等）按回答不合格处理，其余的是服务的问题
                Err(LlmError::InvalidResponse(why)) if correction.is_none() => {
                    correction = Some(format!("the reply was not usable: {why}"));
                    continue;
                }
                Err(e) => return self.failed(llm_error(e)),
            };
            let parsed = pagesift_llm::parse_json(&reply)
                .map_err(|e| format!("not valid JSON: {e}"))
                .and_then(|v| Ok((parse_action(&v)?, v)));
            let (action, raw) = match parsed {
                Ok(a) => a,
                Err(why) => {
                    if correction.is_some() {
                        return self.failed(not_usable(&why));
                    }
                    correction = Some(why);
                    continue;
                }
            };
            let refused = match &action {
                Action::Finish { .. } => None,
                _ if stop.is_some() => {
                    Some("no more tools are available: reply with the finish action")
                }
                Action::Search { .. } if !self.tools.search => {
                    Some("the search tool is not available in this run")
                }
                Action::Map { .. } if !self.tools.map => {
                    Some("the map tool is not available in this run")
                }
                _ => None,
            };
            if let Some(why) = refused {
                if correction.is_some() {
                    return self.failed(not_usable(why));
                }
                correction = Some(why.to_string());
                continue;
            }
            if let Some(n) = raw.get("notes").and_then(Value::as_str) {
                self.notes = protocol::clip(n, self.budget.page_tokens).to_string();
            }
            match action {
                Action::Search { query, limit } => {
                    self.fresh.clear();
                    self.tool_search(&query, limit).await;
                }
                Action::Map { url, search } => {
                    self.fresh.clear();
                    self.last.clear();
                    self.tool_map(&url, search.as_deref()).await;
                }
                Action::Scrape { urls, query } => self.tool_scrape(urls, query).await,
                Action::Extract { url, intent } => self.tool_extract(&url, &intent).await,
                Action::Finish {
                    data,
                    cited,
                    missing,
                    message,
                } => {
                    let schema_valid = self
                        .req
                        .schema
                        .as_ref()
                        .map(|s| pagesift_llm::validate_schema(s, &data));
                    if let Some(Err(errs)) = &schema_valid
                        && correction.is_none()
                    {
                        correction = Some(format!(
                            "`data` does not match the schema: {}",
                            errs.join("; ")
                        ));
                        continue;
                    }
                    let valid = schema_valid.map(|r| r.is_ok());
                    return self.finished(data, &cited, missing, message, valid, stop);
                }
            }
            correction = None;
        }
    }

    fn finished(
        &mut self,
        data: Value,
        cited: &serde_json::Map<String, Value>,
        missing: Vec<String>,
        message: Option<String>,
        schema_valid: Option<bool>,
        stop: Option<Stop>,
    ) -> AgentOutcome {
        let (field_sources, unverified) = provenance::attribute(&data, cited, &self.evidence);
        if !unverified.is_empty() {
            self.warnings.push(format!(
                "agent_unverified_fields: {}",
                unverified.join(", ")
            ));
        }
        let answered = !data.is_null() && missing.is_empty() && schema_valid != Some(false);
        let reason = match stop {
            Some(Stop::Adaptive(v)) => v,
            _ => "answered",
        };
        let mut o = self.outcome(reason);
        o.complete = answered;
        // 预算逼停的回答要求更严：模型没有机会再查，字段也要都核实得了
        if let Some(Stop::Budget(kind)) = stop
            && !(answered && unverified.is_empty())
        {
            o.complete = false;
            o.finish_reason = "budget_exhausted".into();
            o.budget.exhausted = Some(kind.to_string());
        }
        if !o.complete {
            let mut what = vec![];
            if !missing.is_empty() {
                what.push(format!("missing: {}", missing.join(", ")));
            }
            if schema_valid == Some(false) {
                what.push("the answer does not match the schema".to_string());
            }
            if data.is_null() {
                what.push("no answer".to_string());
            }
            o.warnings
                .push(format!("agent_incomplete: {}", what.join("; ")));
        }
        if schema_valid == Some(false) && o.budget.exhausted.is_none() {
            o.error = Some(AgentFailure::from(&not_usable(
                "the answer does not match the schema after one correction",
            )));
        }
        o.data = Some(data);
        o.schema_valid = schema_valid;
        o.field_sources = field_sources;
        o.evidence_spans = provenance::to_evidence_spans(&o.field_sources, &o.evidence);
        o.missing = missing;
        o.message = message;
        o
    }
}

fn not_usable(why: &str) -> ApiError {
    ApiError::new(
        ErrorCode::UpstreamError,
        format!("the LLM reply is not usable after one correction: {why}"),
    )
}
