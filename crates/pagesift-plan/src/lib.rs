//! 执行计划（DESIGN §6.3–§6.6）：结果质量分、执行器选择、校验与按块合并。
//!
//! 0.83 先有质量分（[`quality`]）和文字体系权重（[`script`]）；Planner、Verifier 在 0.87 加入。

pub mod quality;
pub mod script;
