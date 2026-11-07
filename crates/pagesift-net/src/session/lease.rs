//! 一次使用会话的租约：结束时按结果计数，判断会话是否失效。

use std::sync::Arc;

use super::CookieSession;

pub(crate) struct SessionUse {
    pub(super) session: Arc<CookieSession>,
    pub(super) finished: bool,
}
impl SessionUse {
    pub(crate) fn finish(mut self, status: u16) {
        // A rate limit or an upstream outage says nothing about the identity.
        if status == 429 || (500..=599).contains(&status) {
            self.neutral();
        } else {
            self.record(!matches!(status, 401 | 403));
        }
    }
    fn record(&mut self, success: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut s = self.session.lock();
        if self.session.config.exclusive {
            s.in_flight = false;
        }
        if success {
            s.errors = 0;
            s.finished_ok += 1;
        } else {
            s.errors = s.errors.saturating_add(1);
            s.finished_failed += 1;
            // 从文件读回的会话只恢复了连续失败数，本进程的失败总数可能比它少：按历史干净处理
            let before = s.finished_failed.saturating_sub(u64::from(s.errors));
            if invalidated(
                s.errors,
                s.finished_ok,
                before,
                self.session.config.max_consecutive_errors,
            ) {
                s.retired = true;
            }
        }
    }

    fn neutral(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.session.config.exclusive {
            self.session.lock().in_flight = false;
        }
    }
}
/// 会话是否失效：Wald 序贯概率比检验（Ann. Math. Statist. 16, 1945），两类错误率都是 `δ`，
/// 门槛 `A = (1−δ)/δ`。H₁"会话失效"下每次使用都失败；H₀"会话有效"下每次以基线失败率 `p₀`
/// 失败。连续 `k` 次失败的似然比是 `(1/p₀)^k`，`≥ A` 时判失效。
///
/// 先验（先验包 v0）：`p₀ = A^{−1/min_errors}`，恰好让 `k = min_errors`（0.81 的"连续 3 次"）过门槛。
/// 只在显著时更新：本会话在这一串连续失败之前的失败率，其 Wilson 下界（水平 δ）高于先验
/// 时才用它——历史失败率确实高的站点（部分页面本来就 403）要更多次连续失败才判失效；
/// 其余情况（包括冷启动、历史干净）都与 0.81 相同。
pub(super) fn invalidated(errors: u32, ok: u64, failed_before: u64, min_errors: u32) -> bool {
    if errors < min_errors {
        return false;
    }
    let delta = pagesift_stat::Delta::DEFAULT;
    let ln_a = ((1.0 - delta.get()) / delta.get()).ln();
    let prior = (-ln_a / f64::from(min_errors.max(1))).exp();
    let n = (ok + failed_before) as f64;
    let (lower, _) = pagesift_stat::beta::wilson(failed_before as f64, n, delta);
    let p0 = prior.max(lower);
    // [推导] 浮点舍入的容差：先验处 k·ln(1/p₀) 与 ln A 在数学上相等
    f64::from(errors) * (1.0 / p0).ln() >= ln_a * (1.0 - 1e-9)
}

impl Drop for SessionUse {
    fn drop(&mut self) {
        if self.session.config.neutral_on_cancel {
            self.neutral();
        } else {
            self.record(false);
        }
    }
}
