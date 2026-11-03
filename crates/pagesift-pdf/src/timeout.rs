//! 解析时限：由“字节数 → 耗时”的回归给出，替代固定的 30 s。
//!
//! 模型：`ln t = β₀ + β₁·ln b + r`（`t` 秒、`b` 字节），普通最小二乘（一元回归的闭式解，
//! 充分统计量 Σ1、Σx、Σx²、Σy、Σxy、Σy² 可直接相加合并，同 `pagesift_stat::regress`）。
//! 残差 `r` 的上尾用超阈值法（Pickands 1975；Balkema & de Haan 1974）：阈值 `u` 取残差的
//! `q(1−δ)`，超出部分近似指数分布、均值 `m`，于是
//!
//! ```text
//! q(1−ε) = u + m · ln(δ/ε)
//! ```
//!
//! 时限 = `exp(β̂₀ + β̂₁·ln b + q(1−ε))`。`ε` 是杀掉一份本来能解完的文件的概率：对样本里的 `n`
//! 份文件做 Bonferroni，`ε = δ/n`，即“整个参考集上错杀一份的概率不超过 δ”，`ln(δ/ε) = ln n`。
//! 比训练集里最大的文件还大时，不按斜率（< 1）外推，按线性外推（耗时至少和字节数同比增长），
//! 免得大文件被过早截断。用户给的时限（[`crate::Options::timeout`]）是硬上限。
//!
//! 先验包：0.81.5 在 B1（1,269 页）、B2、B3、B5（1 路和 16 路）上解析成功的 1,859 次计时（CLI 计时，
//! 含进程启动，比库内解析更慢，因此偏宽）。`u`、`m` 以残差标准差 `s` 为单位保存，运行时随 `s` 缩放。
//! 留一集交叉验证（拿其余各集拟合、预测留出的那一集）七个集都没有错杀，最小余量 e^1.9 ≈ 6.6 倍
//! （docs/history/0.88_pdf.md）。
//!
//! 在线更新：每个线程一份本地的充分统计量（先验 + 本线程解析成功的计时），不共享、不加锁
//! （工人本地副本）。超时的那些是截尾样本，不进回归（只会让时限偏宽）。

use std::cell::RefCell;
use std::time::Duration;

use pagesift_stat::Delta;

// [推导] 先验包：Σ1、Σx、Σx²、Σy、Σxy、Σy²（x = ln 字节，y = ln 秒），0.81.5 的 1,859 次计时
const PRIOR_N: u64 = 1859;
const PRIOR_SX: f64 = 21982.805389412522; // [推导] 先验包 Σx
const PRIOR_SXX: f64 = 264223.1784182777; // [推导] 先验包 Σx²
const PRIOR_SY: f64 = -9202.769722829255; // [推导] 先验包 Σy
const PRIOR_SXY: f64 = -108112.15389823407; // [推导] 先验包 Σxy
const PRIOR_SYY: f64 = 46382.176479332746; // [推导] 先验包 Σy²
/// [推导] 先验包里最大文件的 ln 字节（15.0 MB）。
const PRIOR_X_MAX: f64 = 16.52662233432514;
/// [推导] 残差的 q(1−δ) 阈值 u 和超出部分的均值 m，以残差标准差为单位（先验包上 δ = 0.05 时）。
const TAIL_U: f64 = 1.973651086589802;
const TAIL_M: f64 = 1.0013189835663623; // [推导] 同上，超阈值部分的均值

/// 一个线程的模型：一元回归的充分统计量和见过的最大文件。
#[derive(Clone)]
pub(crate) struct Model {
    n: f64,
    sx: f64,
    sxx: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
    x_max: f64,
}

/// 拟合结果：`ln t = b0 + b1·x`，残差标准差 `s`。
struct Fit {
    b0: f64,
    b1: f64,
    s: f64,
}

impl Model {
    /// 只有先验包的模型。
    pub(crate) fn prior() -> Model {
        Model {
            n: PRIOR_N as f64,
            sx: PRIOR_SX,
            sxx: PRIOR_SXX,
            sy: PRIOR_SY,
            sxy: PRIOR_SXY,
            syy: PRIOR_SYY,
            x_max: PRIOR_X_MAX,
        }
    }

    pub(crate) fn observe(&mut self, bytes: usize, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        if bytes == 0 || secs <= 0.0 {
            return;
        }
        let (x, y) = ((bytes as f64).ln(), secs.ln());
        self.n += 1.0;
        self.sx += x;
        self.sxx += x * x;
        self.sy += y;
        self.sxy += x * y;
        self.syy += y * y;
        self.x_max = self.x_max.max(x);
    }

    /// 正规方程的闭式解；残差方差 `s² = (Σy² − b0·Σy − b1·Σxy)/(n − 2)`。
    fn fit(&self) -> Option<Fit> {
        let d = self.n * self.sxx - self.sx * self.sx;
        if self.n <= 2.0 || d.abs() < f64::EPSILON * self.n * self.sxx {
            return None;
        }
        let b1 = (self.n * self.sxy - self.sx * self.sy) / d;
        let b0 = (self.sy - b1 * self.sx) / self.n;
        let s2 = (self.syy - b0 * self.sy - b1 * self.sxy) / (self.n - 2.0);
        Some(Fit {
            b0,
            b1,
            s: s2.max(0.0).sqrt(),
        })
    }

    /// 字节数为 `bytes` 的文件的时限。
    pub(crate) fn timeout(&self, bytes: usize, delta: Delta) -> Duration {
        let Some(fit) = self.fit() else {
            return Duration::MAX;
        };
        let (s, n) = (fit.s, self.n);
        // 阈值 u 是 q(1−δ)；先验包按 δ = 0.05 定的，换 δ 时按指数尾平移：q(1−δ') = u + m·ln(0.05/δ')
        let u = TAIL_U + TAIL_M * (Delta::DEFAULT.get() / delta.get()).ln();
        let tail = s * (u + TAIL_M * n.ln());
        let x = (bytes.max(1) as f64).ln();
        let line = |x: f64| fit.b0 + fit.b1 * x + tail;
        let ln_t = if x > self.x_max {
            // 超出见过的范围：从最大文件处按线性外推（ln t 随 ln b 的斜率取 1）
            line(self.x_max) + (x - self.x_max)
        } else {
            line(x)
        };
        Duration::try_from_secs_f64(ln_t.exp()).unwrap_or(Duration::MAX)
    }
}

thread_local! {
    static MODEL: RefCell<Model> = RefCell::new(Model::prior());
}

/// 这个线程上，一份 `bytes` 字节的 PDF 的解析时限；`cap` 是用户给的上限。
pub fn parse_timeout(bytes: usize, cap: Option<Duration>) -> Duration {
    let t = MODEL.with(|m| m.borrow().timeout(bytes, Delta::DEFAULT));
    cap.map_or(t, |c| c.min(t))
}

/// 记下一次解析成功的耗时（[`crate::extract_with`] 已经自动记；别的路径解析的调用方可以自己记）。
pub fn observe_parse(bytes: usize, elapsed: Duration) {
    MODEL.with(|m| m.borrow_mut().observe(bytes, elapsed));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prior_matches_the_recorded_fit() {
        let m = Model::prior();
        let fit = m.fit().unwrap();
        // 先验包的拟合：β₀ ≈ −6.918、β₁ ≈ 0.166、s ≈ 0.617（见模块注释）
        assert!((fit.b0 + 6.9178).abs() < 1e-3, "{}", fit.b0);
        assert!((fit.b1 - 0.16637).abs() < 1e-4);
        assert!((fit.s - 0.61684).abs() < 1e-4);
    }

    #[test]
    fn timeouts_grow_with_size_and_stay_generous() {
        let m = Model::prior();
        let d = Delta::DEFAULT;
        let small = m.timeout(1_000, d);
        let mid = m.timeout(1_000_000, d);
        let big = m.timeout(15_000_000, d);
        let huge = m.timeout(150_000_000, d);
        assert!(small < mid && mid < big && big < huge);
        // 先验包里最慢的一次（8 MB、0.39 s）离时限还有几倍余量
        assert!(m.timeout(8_224_901, d) > Duration::from_millis(390 * 6));
        // 超出范围按线性外推：大十倍，时限也大十倍
        let ratio = huge.as_secs_f64() / big.as_secs_f64();
        assert!((ratio - 10.0).abs() < 0.1, "{ratio}");
    }

    /// 机器变慢：本线程观测到的耗时整体变长，时限跟着变长（在线更新）。
    #[test]
    fn learns_a_slower_machine() {
        let mut m = Model::prior();
        let before = m.timeout(100_000, Delta::DEFAULT);
        for i in 0..4000 {
            let b = 10_000 + i * 997;
            m.observe(b, Duration::from_secs(2));
        }
        assert!(m.timeout(100_000, Delta::DEFAULT) > before * 3);
    }

    #[test]
    fn cap_wins() {
        assert_eq!(
            parse_timeout(1 << 20, Some(Duration::from_millis(5))),
            Duration::from_millis(5)
        );
        assert!(parse_timeout(1 << 20, None) > Duration::from_secs(1));
    }

    /// 更小的 δ（更不愿意错杀）给更长的时限。
    #[test]
    fn smaller_delta_is_more_lenient() {
        let m = Model::prior();
        let tight = m.timeout(1 << 20, Delta::new(0.2).unwrap());
        let loose = m.timeout(1 << 20, Delta::new(0.01).unwrap());
        assert!(loose > tight);
    }
}
