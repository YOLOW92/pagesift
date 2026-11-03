//! 推测执行（DESIGN §6.6）：HTTP 正文到手、还没提取时，按条件同时启动浏览器，
//! 省掉"提取 + 决策"这段等待。
//!
//! 条件（§6.6 `P(需要渲染)·E[可省的等待]·v_t > λ_browser·E[标签秒]`，改写成同一量纲，单位都是秒）：
//!
//! ```text
//! 推测 ⇔ p · s  >  λ · (1 − p) · (s + c)
//! ```
//!
//! - `p`：这个路径族"渲染有增益"的后验均值（planner 的 Beta 计数；族按首块里有没有 `<script` 分）；
//! - `s`：可省的等待 = 这么大的 HTML 预计的提取耗时（提取耗时 / 字节的窗口比值，[`super::render_stats`]）；
//! - `c`：推测白做时多占的标签时间里与 `s` 无关的部分，取该内核拿到文档耗时的 `q(δ)` 下界（`min_attempt`）；
//! - `λ`：浏览器标签的影子价格，按统一准入的利用率 `ρ`（`pagesift_flow::Admission`）取
//!   `shadow_price(ρ, W) / W = 1/(1−ρ)`（M/M/1 的逗留时间放大倍数，Kleinrock 1975 §3.2）：
//!   空闲时一标签秒就值一秒，饱和时无穷大（不推测）；
//! - `v_t`：单个请求自己的秒，记 1（批量和爬取的吞吐边际值还没有估计量，这里不区分）。
//!
//! 冷启动（族里还没有数据、或还没有提取耗时的估计）不推测，行为等于 0.81。
//! 推测出去的渲染在决策前不知道 HTTP 正文有多少：按"HTTP 为空"跑（不设导航上限、不早停），
//! 提取完成后探针才拿到基线开始工作，所以结果只会比非推测的更完整、不会更薄。

use std::sync::atomic::{AtomicU64, Ordering};

use pagesift_api::types::PlanStep;

use super::*;

/// [协议] 首块嗅探的窗口：WHATWG Encoding Standard 的 meta 预扫字节数（同 pagesift-net 的 charset 预扫）
const SNIFF_HEAD: usize = 4096;

/// 基线还不知道（提取没完成）。
pub(super) const BASELINE_UNKNOWN: u64 = u64::MAX;

/// 自动升级的早停探针：当前 DOM 提取出的正文不比 HTTP 的多就可以停。
/// `baseline` 是 HTTP 正文的加权字数；还是 [`BASELINE_UNKNOWN`] 时探针不下结论（继续等）。
pub(super) fn render_probe(
    opts: &ScrapeOptions,
    base: Url,
    baseline: Arc<AtomicU64>,
) -> Arc<dyn Fn(String) -> pagesift_cdp::ProbeFuture + Send + Sync> {
    let only_main_content = opts.only_main_content;
    Arc::new(move |html: String| -> pagesift_cdp::ProbeFuture {
        let copts = pagesift_content::Options {
            base_url: Some(base.clone()),
            only_main_content,
            status: None,
            detect_barrier: false,
            ..Default::default()
        };
        let baseline = baseline.clone();
        Box::pin(async move {
            let known = baseline.load(Ordering::Relaxed);
            if known == BASELINE_UNKNOWN {
                return false;
            }
            cpu_task(move || {
                let doc = CONTENT_WORKSPACE.with(|ws| {
                    pagesift_content::process_with_workspace(&html, &copts, &mut ws.borrow_mut())
                });
                text_weight(&doc.markdown) as u64 <= known
            })
            .await
            .unwrap_or(false)
        })
    })
}

impl Scraper {
    /// 是否推测执行（见模块说明）。`body` 是 HTTP 响应正文。
    pub(super) fn plan_speculate(&self, url: &str, body: &[u8]) -> (bool, PlanStep) {
        let mut step = PlanStep {
            action: "speculate".into(),
            ..PlanStep::default()
        };
        let head = &body[..body.len().min(SNIFF_HEAD)];
        let script = pagesift_flow::sniff::has_script_tag(&String::from_utf8_lossy(head));
        let p = self
            .planner
            .mean(&super::planner::render_family(url, script));
        let saved = self.render_stats.extract_estimate(body.len());
        let (Some(p), Some(saved)) = (p, saved) else {
            return (false, step);
        };
        let engine = self
            .browsers
            .first()
            .and_then(|(c, _)| c.engine)
            .unwrap_or(pagesift_cdp::EngineKind::Chrome);
        let c = self.render_stats.min_attempt(engine).as_secs_f64();
        let (go, value, charge) = speculate_rule(
            p,
            saved.as_secs_f64(),
            c,
            self.render_stats.tab_utilization(),
        );
        step.chosen = go;
        step.learned = true;
        step.p = Some(p);
        step.value = Some(value);
        step.charge = Some(charge);
        (go, step)
    }
}

/// `p·s > λ(ρ)·(1−p)·(s+c)`，`λ(ρ) = 1/(1−ρ)`；返回 (是否推测, 收益, 代价)。
fn speculate_rule(p: f64, s: f64, c: f64, rho: f64) -> (bool, f64, f64) {
    let price = if rho >= 1.0 {
        f64::INFINITY
    } else {
        1.0 / (1.0 - rho.max(0.0))
    };
    let value = p * s;
    let charge = price * (1.0 - p) * (s + c);
    (value > charge, value, charge)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空闲时只要推测的期望收益超过白做的标签时间就开；同样的页在饱和时不开。
    #[test]
    fn triggers_follow_the_formula_under_idle_and_saturated_load() {
        // 几乎总要渲染的族：p = 0.98，提取 50 ms，启动 300 ms
        let (idle, v, ch) = speculate_rule(0.98, 0.05, 0.3, 0.0);
        assert!(idle && (v - 0.049).abs() < 1e-12 && (ch - 0.007).abs() < 1e-12);
        let (busy, ..) = speculate_rule(0.98, 0.05, 0.3, 0.9);
        assert!(!busy);
        assert!(!speculate_rule(0.98, 0.05, 0.3, 1.0).0);
        // 一半页面要渲染的族：空闲时也不值得（白做的启动时间远多于省下的提取时间）
        assert!(!speculate_rule(0.5, 0.05, 0.3, 0.0).0);
        // 边界：p·s = (1−p)(s+c) ⇔ p = (s+c)/(2s+c)
        let p = 0.35 / 0.4;
        assert!(!speculate_rule(p - 1e-6, 0.05, 0.3, 0.0).0);
        assert!(speculate_rule(p + 1e-6, 0.05, 0.3, 0.0).0);
    }
}
