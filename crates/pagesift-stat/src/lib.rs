//! 可合并的在线统计与决策规则。
//!
//! 所有估计器都实现 [`Stat`]：线程之间、机器之间、时间段之间的汇总是同一个 `merge`，
//! 与合并顺序无关。不依赖其他 pagesift crate，也不依赖第三方 crate。
//!
//! 唯一的元参数是风险水平 [`Delta`]；其余调优值都由数据和公式推出。

pub mod beta;
pub mod calib;
pub mod change;
pub mod codec;
pub mod countmin;
pub mod decide;
pub mod hazard;
pub mod hll;
pub mod limit;
pub mod moments;
pub mod poisson;
pub mod regress;
pub mod rng;
pub mod shrink;
pub mod size_time;
pub mod sketch;
pub mod special;
pub mod survival;
pub mod table;
pub mod usl;
pub mod verdict;
pub mod window;

pub use codec::{DecodeError, Reader, Writer};

/// 可合并、可编码的统计量。
pub trait Stat: Default + Clone + Send + Sync {
    /// 把 `other` 并进来。必须满足结合律和交换律（结果与合并顺序无关）。
    fn merge(&mut self, other: &Self);
    /// 追加二进制编码。
    fn encode(&self, out: &mut Writer);
    /// 从编码读回。
    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError>;
}

/// 全局风险水平 `δ`：分位数上界用 `q(1−δ)`，检验的显著性水平是 `δ`，
/// SPRT 的两类错误率都是 `δ`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delta(f64);

impl Delta {
    /// [δ] 默认 0.05：统计决策惯用的显著性水平（Fisher 1925 以来的约定），用户可改。
    pub const DEFAULT: Delta = Delta(0.05);

    /// `δ` 必须在 (0, 1/2) 内：大于 1/2 时"上界"会落在中位数以下，失去意义。
    pub fn new(delta: f64) -> Option<Self> {
        (delta > 0.0 && delta < 0.5).then_some(Self(delta))
    }

    pub fn get(self) -> f64 {
        self.0
    }

    /// 单侧标准正态分位数 `z(1−δ)`。
    pub fn z(self) -> f64 {
        special::normal_quantile(1.0 - self.0)
    }
}

impl Default for Delta {
    fn default() -> Self {
        Self::DEFAULT
    }
}
