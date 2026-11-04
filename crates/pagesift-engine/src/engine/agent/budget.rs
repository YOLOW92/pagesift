//! agent 的预算：`effort` 决定页数上限，其余上限由页数和模型限额推出；按每分钟 token 限额控速。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use pagesift_api::types::{AdaptiveOptions, AgentBudget, AgentEffort, AgentRequest};
use pagesift_llm::ChatClient;

/// [δ] 全局风险水平。
const DELTA: f64 = 0.05;
/// [策略] 相邻两档 effort 的页数比：low 的页数等于一个 patience（连续这么多页没有新内容才算饱和），
/// medium = 4 倍，等于 `/v2/extract` 的页数上限 20；high 再乘 4。
const EFFORT_STEP: usize = 4;
/// [策略] 服务不报上下文长度时的先验：8192（OpenAI 兼容服务里常见的最小上下文，和 0.97 相同）。
const DEFAULT_CONTEXT: usize = 8192;
/// [协议] 服务的 token 限额按分钟计（`x-ratelimit-*-tokens`）。
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// 这一档的页数上限。
fn effort_pages(effort: Option<AgentEffort>) -> usize {
    let low = AdaptiveOptions::default().patience.max(1);
    match effort {
        Some(AgentEffort::Low) => low,
        Some(AgentEffort::Medium) | None => low * EFFORT_STEP,
        Some(AgentEffort::High) => low * EFFORT_STEP * EFFORT_STEP,
    }
}

/// 一次调用能用的 token：`(提示上限, 输出上限)`。取模型上下文和每分钟限额中较小的，只用 1 − δ
/// （估计有误差）；不知道输入、输出的比例，对称划分，模型有更小的输出上限时输出取它，剩下的都给提示。
pub(super) fn call_limits(
    context: usize,
    max_completion: Option<usize>,
    per_minute: Option<usize>,
) -> (usize, usize) {
    let usable = (per_minute.map_or(context, |t| t.min(context)) as f64 * (1.0 - DELTA)) as usize;
    let half = usable / 2;
    let output = max_completion.map_or(half, |o| o.min(half));
    (usable - output, output)
}

/// 一次运行的预算。
#[derive(Debug, Clone)]
pub(super) struct Budget {
    pub max_pages: u64,
    pub max_calls: u64,
    pub max_tokens: u64,
    pub max_credits: Option<u64>,
    pub input: usize,
    pub output: usize,
    /// 服务报的每分钟 token 限额。
    pub per_minute: Option<usize>,
    /// 一页证据的 token 上限：最后一次回答的提示要放得下至少两页（`Adaptive` 的 MIN_SOURCES：覆盖
    /// 要两页以上的证据），另一半留给任务、步骤记录和笔记，所以是提示上限的 1/4。
    pub page_tokens: usize,
}

impl Budget {
    pub async fn new(llm: &ChatClient, req: &AgentRequest) -> Self {
        let (context, completion) = llm.model_limits().await.unwrap_or((DEFAULT_CONTEXT, None));
        let per_minute = llm.token_rate_limit().await;
        let (input, output) = call_limits(context, completion, per_minute);
        Self {
            per_minute,
            ..Self::from_limits(input, output, req)
        }
    }

    pub fn from_limits(input: usize, output: usize, req: &AgentRequest) -> Self {
        let pages = effort_pages(req.effort);
        // [推导] 每页最多单独规划一次、再规划一次怎么找到它，加上最后回答和一次纠正
        let calls = 2 * pages + 2;
        Self {
            max_pages: pages as u64,
            max_calls: calls as u64,
            // 每次调用都用满上限时的总量：平时碰不到，服务报的实际用量远超估计时才生效
            max_tokens: (calls * (input + output)) as u64,
            max_credits: req.max_credits,
            input,
            output,
            per_minute: None,
            page_tokens: (input / 4).max(1), // [推导] 见字段说明
        }
    }

    pub fn report(&self, exhausted: Option<&str>) -> AgentBudget {
        AgentBudget {
            max_pages: self.max_pages,
            max_llm_calls: self.max_calls,
            max_tokens: self.max_tokens,
            max_credits: self.max_credits,
            input_tokens_per_call: self.input as u64,
            exhausted: exhausted.map(str::to_string),
        }
    }
}

/// 按每分钟 token 限额控速：这一分钟里用掉的加上这次要用的超过限额时，等最早的那些过期。
/// 只算本次运行自己的用量；别的运行造成的 429 由客户端按 Retry-After 重试。
#[derive(Debug, Default)]
pub(super) struct Pacer {
    limit: Option<usize>,
    used: VecDeque<(Instant, usize)>,
}

impl Pacer {
    pub fn new(limit: Option<usize>) -> Self {
        Self {
            limit,
            used: VecDeque::new(),
        }
    }

    /// 这次调用（估计用 `tokens`）要等多久。
    pub fn wait_for(&mut self, tokens: usize, now: Instant) -> Duration {
        let Some(limit) = self.limit else {
            return Duration::ZERO;
        };
        while self
            .used
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) >= RATE_WINDOW)
        {
            self.used.pop_front();
        }
        let mut in_window: usize = self.used.iter().map(|(_, n)| n).sum();
        for (t, n) in &self.used {
            if in_window + tokens <= limit {
                return Duration::ZERO;
            }
            in_window -= n;
            if in_window + tokens <= limit {
                return RATE_WINDOW.saturating_sub(now.duration_since(*t));
            }
        }
        Duration::ZERO
    }

    pub fn record(&mut self, tokens: usize, now: Instant) {
        self.used.push_back((now, tokens));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(effort: Option<AgentEffort>) -> AgentRequest {
        AgentRequest {
            prompt: "p".into(),
            urls: None,
            schema: None,
            effort,
            strict_constrain_to_urls: false,
            max_credits: None,
            webhook: None,
            allowed_hosts: None,
        }
    }

    #[test]
    fn effort_changes_only_the_caps() {
        let low = Budget::from_limits(3800, 3800, &req(Some(AgentEffort::Low)));
        let med = Budget::from_limits(3800, 3800, &req(None));
        let high = Budget::from_limits(3800, 3800, &req(Some(AgentEffort::High)));
        assert_eq!((low.max_pages, med.max_pages, high.max_pages), (5, 20, 80));
        assert_eq!((low.max_calls, med.max_calls), (12, 42));
        assert_eq!(low.max_tokens, 12 * 7600);
        assert_eq!(low.page_tokens, high.page_tokens);
    }

    #[test]
    fn limits_respect_rate_context_and_completion_cap() {
        // Groq 免费档：上下文 131072、每分钟 8000
        assert_eq!(call_limits(131_072, Some(65_536), Some(8000)), (3800, 3800));
        // 输出上限更小：多出来的给提示，max_tokens 不超过模型上限
        assert_eq!(call_limits(100_000, Some(1024), None), (93_976, 1024));
        assert_eq!(call_limits(8192, None, None), (3891, 3891));
    }

    #[test]
    fn pacer_waits_for_the_oldest_to_expire() {
        // wall-clock: 只取一个起点做加减，没有等待
        let t0 = Instant::now();
        let mut p = Pacer::new(Some(1000));
        assert_eq!(p.wait_for(600, t0), Duration::ZERO);
        p.record(600, t0);
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(p.wait_for(300, t1), Duration::ZERO);
        p.record(300, t1);
        // 900 + 300 超限：等第一笔过期，还剩 50 秒
        assert_eq!(
            p.wait_for(300, t0 + Duration::from_secs(10)),
            Duration::from_secs(50)
        );
        // 过期后不用等
        assert_eq!(
            p.wait_for(300, t0 + Duration::from_secs(61)),
            Duration::ZERO
        );
        // 没有限额时不控速
        assert_eq!(Pacer::new(None).wait_for(1 << 40, t0), Duration::ZERO);
    }
}
