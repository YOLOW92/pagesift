//! `pagesift agent "PROMPT"`：和 `POST /v2/agent` 同一个循环，跑完在 stdout 输出一个 JSON，
//! 字段和 `GET /v2/agent/{id}` 相同；每一步写到 stderr。结果不完整或失败时退出码为 1。

use crate::*;

pub(crate) struct AgentArgs {
    pub prompt: String,
    pub urls: Vec<String>,
    pub schema: Option<std::path::PathBuf>,
    pub effort: Option<String>,
    pub strict_constrain_to_urls: bool,
    pub max_credits: Option<u64>,
    pub allowed_hosts: Vec<String>,
    pub include_evidence: bool,
    pub allow: AllowCidrs,
}

struct Progress;

impl pagesift::api::AgentSink for Progress {
    fn step<'a>(
        &'a self,
        step: &'a pagesift::api::types::AgentStep,
        credits: u64,
    ) -> pagesift::api::BoxFuture<'a, ()> {
        Box::pin(async move {
            eprintln!(
                "turn {}: {} {} -> {} ({credits} credits)",
                step.turn, step.tool, step.input, step.result
            );
        })
    }
}

pub(crate) fn run(args: AgentArgs, cfg: pagesift::config::Config) -> Result<ExitCode> {
    let schema = match &args.schema {
        Some(path) => {
            let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            Some(
                serde_json::from_slice::<serde_json::Value>(&raw)
                    .with_context(|| format!("parsing {}", path.display()))?,
            )
        }
        None => None,
    };
    let req: pagesift::api::types::AgentRequest = serde_json::from_value(serde_json::json!({
        "prompt": args.prompt,
        "urls": (!args.urls.is_empty()).then_some(&args.urls),
        "schema": schema,
        "effort": args.effort,
        "strictConstrainToURLs": args.strict_constrain_to_urls,
        "maxCredits": args.max_credits,
        "allowedHosts": (!args.allowed_hosts.is_empty()).then_some(&args.allowed_hosts),
    }))?;
    pagesift::api::check_agent_request(&req).map_err(|e| anyhow::anyhow!("{}", e.message))?;
    light_runtime()?.block_on(async move {
        let mut sc = cfg.scraper(false)?;
        args.allow.apply(&mut sc);
        let scraper = pagesift::Scraper::new(sc)?;
        use pagesift::api::Engine;
        scraper
            .agent_check(&req)
            .map_err(|e| anyhow::anyhow!("{}", e.message))?;
        let outcome = scraper.agent(&req, std::sync::Arc::new(Progress)).await;
        let body = if args.include_evidence {
            pagesift::api::agent_body_with_evidence(&outcome)
        } else {
            pagesift::api::agent_body(&outcome)
        };
        println!("{body}");
        scraper.shutdown().await;
        Ok(if body["status"] == "completed" {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        })
    })
}
