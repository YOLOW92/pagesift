//! 自适应 DOM 元素自愈重定位引擎（对标 Scrapling）。
//!
//! 在前端代码重构、CSS 类名 Hash 混淆、HTML 标签变更与 DOM 容器包裹的极端环境下，
//! 通过多维拓扑指纹比对实现微秒级高精度元素自愈找回。

mod fingerprint;
mod similarity;
mod tracker;

pub use fingerprint::ElementFingerprint;
pub use similarity::score_similarity;
pub use tracker::{RelocateOutcome, RelocationOptions, relocate};
