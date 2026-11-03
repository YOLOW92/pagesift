//! `WaitUntil::MutationIdle`：等 DOM 变化停下来（Jina 的 `x-respond-timing: mutation-idle`）。
//!
//! “静默多久算稳定”从这个页面上实际观测到的变化间隔推出：把还在变化的页面上相邻两次变化的间隔
//! 看作指数分布（变化是泊松过程），速率 λ 的最大似然估计是 1 / 平均间隔。页面仍在变化时，
//! t 时间内一次也不变的概率是 e^{−λt}；取 t = ln(1/δ) / λ̂，静默超过 t 时“其实还在变、只是碰巧没变”
//! 的概率不超过 δ（唯一的元参数 δ = 0.05）。还没观测到两次变化时，用到目前为止已过去的时间作为
//! 间隔的保守估计。整个等待受网络就绪的学习上限约束（和 `NetworkIdle` 共用）。

use std::time::{Duration, Instant};

use super::actions::eval_value;
use super::{CONTENT_POLL, PageCtx};

/// [δ] 全局风险水平。
const DELTA: f64 = 0.05;

/// 注入观察器：记下每次变化的时间（`performance.now()`，毫秒）。重复注入不重复记。
const OBSERVE: &str = "(() => { if (!window.__psMut) { window.__psMut = [performance.now()]; \
    new MutationObserver(() => window.__psMut.push(performance.now())).observe(document, \
    { subtree: true, childList: true, characterData: true, attributes: true }); } \
    return window.__psMut.length; })()";
/// 读出变化时间和现在的时间。
const READ: &str = "(() => [performance.now(), window.__psMut || []])()";

/// 由变化时间（毫秒，升序，首项是开始观察的时刻）和现在算出：还要等多久（`None` = 已经稳定）。
pub(super) fn remaining(times: &[f64], now: f64) -> Option<Duration> {
    let (first, last) = (*times.first()?, *times.last()?);
    let gaps = times.len().saturating_sub(1);
    // 平均间隔：有变化时用观测到的间隔；只有起点时，已过去的时间是间隔的下界
    let mean = if gaps >= 1 {
        (last - first) / gaps as f64
    } else {
        now - first
    };
    let window = (1.0 / DELTA).ln() * mean.max(0.0);
    let quiet = now - last;
    (quiet < window).then(|| Duration::from_secs_f64(((window - quiet) / 1000.0).max(0.0)))
}

pub(super) async fn wait_mutation_idle(ctx: &PageCtx<'_>) {
    let learn = &ctx.shared.waits.net_idle;
    let started = Instant::now();
    let deadline = started + learn.cap();
    if eval_value(ctx.session, OBSERVE).await.is_err() {
        return;
    }
    loop {
        tokio::time::sleep(CONTENT_POLL).await;
        let Ok(v) = eval_value(ctx.session, READ).await else {
            return;
        };
        let now = v[0].as_f64().unwrap_or_default();
        let times: Vec<f64> = v[1]
            .as_array()
            .map(|a| a.iter().filter_map(|t| t.as_f64()).collect())
            .unwrap_or_default();
        match remaining(&times, now) {
            None => {
                learn.done(started.elapsed(), learn.prior());
                return;
            }
            Some(_) if Instant::now() >= deadline => {
                learn.cut(started.elapsed(), learn.prior());
                return;
            }
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_scales_with_the_observed_gap() {
        // 每 100 ms 变一次，已静默 200 ms：窗口 ln 20 × 100 ≈ 300 ms，还要再等
        let busy = [0.0, 100.0, 200.0, 300.0];
        assert!(remaining(&busy, 500.0).is_some());
        // 静默 400 ms > 300 ms：稳定
        assert!(remaining(&busy, 700.0).is_none());
        // 一次都没变过：起点之后过去的时间就是间隔的下界，第一次检查时 quiet == mean < window
        assert!(remaining(&[0.0], 250.0).is_some());
    }
}
