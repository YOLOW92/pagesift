//! 执行计划：结果质量分、执行器选择、校验与按块合并。
//!
//! - [`quality`]、[`script`]：质量分与文字体系权重（0.83）；
//! - [`block`]、[`merge`]：块与按块合并；
//! - [`plan`]：Planner 与 Verifier 的追加判定；
//! - [`embedded_json`]：从页面内嵌 JSON 取正文的执行器；
//! - [`template`]：族内模板概率。

pub mod block;
pub mod embedded_json;
pub mod learn;
pub mod merge;
pub mod plan;
pub mod quality;
pub mod render;
pub mod script;
pub mod template;
