//! 有界运行长度窗口的贝叶斯在线变化点检测（BOCPD，Beta–Bernoulli 共轭序列）。
//!
//! R. P. Adams & D. J. C. MacKay, "Bayesian Online Changepoint Detection", arXiv:0710.3742, 2007。
//! 针对二元接地命中率/核验成功率序列，使用常数危险率 `H` 与截断上界 `MAX_RUN = 64` 维护
//! 运行长度后验 `P(r_t | x_{1:t})`；作为 [`crate::change::ChangeDetector`]（Page–Hinkley CUSUM）
//! 在有界二元反馈上的可选对照检测器。

use crate::Delta;

// [策略] 截断最大运行长度 64，保证单步更新 O(1) 常数栈开销且覆盖典型在线自适应窗口
const MAX_RUN: usize = 64;
// [策略] 至少积累 8 步稳定观测后才判定新变化点，避免冷启动初期后验未收敛误报
const MIN_MATURE_RUN: usize = 8;
// [推导] 短运行长度（0..=16）累积后验质量衡量最近 16 步内发生体制切换（含伯努利偶发噪声容忍）的后验概率
const SHORT_RUN_HORIZON: usize = 16;
// [策略] 非有限危险率输入回退默认 1/50 = 0.02
const DEFAULT_HAZARD: f64 = 0.02;

/// Beta–Bernoulli 贝叶斯在线变化点检测器。
#[derive(Debug, Clone)]
pub struct BernoulliBocpd {
    hazard: f64,
    prior_alpha: f64,
    prior_beta: f64,
    delta: Delta,
    probs: [f64; MAX_RUN],
    alphas: [f64; MAX_RUN],
    betas: [f64; MAX_RUN],
    active_len: usize,
    steps_since_reset: usize,
}

impl BernoulliBocpd {
    pub fn new(hazard: f64, delta: Delta) -> Self {
        let h = if hazard.is_finite() {
            // [策略] 危险率限制在 [1/1000, 1/4] 之间
            hazard.clamp(0.001, 0.25)
        } else {
            DEFAULT_HAZARD
        };
        let mut probs = [0.0; MAX_RUN];
        probs[0] = 1.0;
        let alphas = [1.0; MAX_RUN];
        let betas = [1.0; MAX_RUN];
        Self {
            hazard: h,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            delta,
            probs,
            alphas,
            betas,
            active_len: 1,
            steps_since_reset: 0,
        }
    }

    /// 输入一次二元观测（`true` = 成功/接地命中，`false` = 失败），返回是否检测到显著体制跳变。
    pub fn push(&mut self, success: bool) -> bool {
        let mut next_probs = [0.0; MAX_RUN];
        let mut next_alphas = [self.prior_alpha; MAX_RUN];
        let mut next_betas = [self.prior_beta; MAX_RUN];

        let mut cp_mass = 0.0;
        let prev_map_run = self.map_run_length();

        let pred_prior = if success {
            self.prior_alpha / (self.prior_alpha + self.prior_beta)
        } else {
            self.prior_beta / (self.prior_alpha + self.prior_beta)
        };

        for r in 0..self.active_len {
            let p_r = self.probs[r];
            // 剪枝非正或次正规（subnormal）极微分支，防止下溢与硬件次正规浮点停顿
            if !(p_r >= f64::MIN_POSITIVE && p_r.is_finite()) {
                continue;
            }
            let a = self.alphas[r].max(f64::MIN_POSITIVE);
            let b = self.betas[r].max(f64::MIN_POSITIVE);
            let denom = a + b;
            if !denom.is_finite() || denom <= 0.0 {
                continue;
            }
            let pred = if success { a / denom } else { b / denom };
            let joint_growth = p_r * pred;

            // Adams & MacKay (2007): changepoint branch evaluates predictive likelihood under fresh prior!
            cp_mass += p_r * pred_prior * self.hazard;
            let next_r = (r + 1).min(MAX_RUN - 1);
            let branch_mass = joint_growth * (1.0 - self.hazard);
            let prev_bucket_mass = next_probs[next_r];
            next_probs[next_r] += branch_mass;
            // 若在截断边界 MAX_RUN - 1 发生合并，保留后验质量占优分支（长稳态运行段）的充分统计量
            if prev_bucket_mass == 0.0 || branch_mass >= prev_bucket_mass {
                next_alphas[next_r] = a + f64::from(u8::from(success));
                next_betas[next_r] = b + f64::from(u8::from(!success));
            }
        }
        next_probs[0] = cp_mass;
        next_alphas[0] = self.prior_alpha + f64::from(u8::from(success));
        next_betas[0] = self.prior_beta + f64::from(u8::from(!success));

        let norm: f64 = next_probs.iter().sum();
        if norm >= f64::MIN_POSITIVE && norm.is_finite() {
            let inv_norm = 1.0 / norm;
            for p in &mut next_probs {
                let scaled = *p * inv_norm;
                *p = if scaled >= f64::MIN_POSITIVE {
                    scaled
                } else {
                    0.0
                };
            }
        } else {
            // 数值下溢恢复：安全重置到初始冷启动先验状态，绝不伪造突变报警
            self.probs = [0.0; MAX_RUN];
            self.probs[0] = 1.0;
            self.alphas = [self.prior_alpha; MAX_RUN];
            self.betas = [self.prior_beta; MAX_RUN];
            self.active_len = 1;
            self.steps_since_reset = 0;
            return false;
        }

        self.probs = next_probs;
        self.alphas = next_alphas;
        self.betas = next_betas;
        self.active_len = (self.active_len + 1).min(MAX_RUN);
        self.steps_since_reset = self.steps_since_reset.saturating_add(1);

        let no_cp_run = self.steps_since_reset.min(MAX_RUN - 1);
        let horizon = SHORT_RUN_HORIZON.min(no_cp_run.saturating_sub(1));
        let recent_shift_mass: f64 = if no_cp_run > 0 {
            self.probs[0..=horizon.min(self.active_len - 1)]
                .iter()
                .sum()
        } else {
            0.0
        };
        // [δ] 当此前存在成熟平稳段（>= MIN_MATURE_RUN）且近期切换后验概率超过 1 - 2δ 时触发重置报警
        let threshold = (1.0 - 2.0 * self.delta.get()).clamp(0.5, 0.99);
        if self.steps_since_reset >= MIN_MATURE_RUN
            && (prev_map_run >= MIN_MATURE_RUN || self.steps_since_reset >= MIN_MATURE_RUN * 2)
            && recent_shift_mass > threshold
        {
            self.reset_to_current(success);
            return true;
        }
        false
    }

    /// 当前最大后验（MAP）运行长度。
    pub fn map_run_length(&self) -> usize {
        let mut best_r = 0;
        // [推导] 概率非负，初始哨兵设为 -1.0 确保第 0 桶恒可成为候选
        let mut best_p = -1.0;
        for r in 0..self.active_len {
            if self.probs[r] > best_p {
                best_p = self.probs[r];
                best_r = r;
            }
        }
        best_r
    }

    fn reset_to_current(&mut self, success: bool) {
        self.probs = [0.0; MAX_RUN];
        self.probs[1] = 1.0;
        self.alphas = [self.prior_alpha; MAX_RUN];
        self.betas = [self.prior_beta; MAX_RUN];
        self.alphas[1] = self.prior_alpha + f64::from(u8::from(success));
        self.betas[1] = self.prior_beta + f64::from(u8::from(!success));
        self.active_len = 2;
        self.steps_since_reset = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_abrupt_regime_drop_without_false_alarm_on_steady_stream() {
        let mut det = BernoulliBocpd::new(0.02, Delta::DEFAULT);
        for _ in 0..30 {
            assert!(!det.push(true));
        }
        assert!(det.map_run_length() >= 20);

        let mut fired = false;
        for _ in 0..8 {
            if det.push(false) {
                fired = true;
                break;
            }
        }
        assert!(fired, "expected BOCPD to detect regime collapse");
    }

    #[test]
    fn guards_nan_hazard_and_long_stream_without_underflow() {
        let mut det = BernoulliBocpd::new(f64::NAN, Delta::DEFAULT);
        for _ in 0..500 {
            assert!(!det.push(true));
        }
        let sum: f64 = det.probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "posterior sum={sum}");
        assert_eq!(det.map_run_length(), MAX_RUN - 1);
    }
}
