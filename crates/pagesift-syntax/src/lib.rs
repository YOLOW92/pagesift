//! 与 tree-sitter 语法兼容的纯 Rust 解析运行时与代码大纲提取（直接重导出 [`yolowsyntax`]）。
//!
//! 语法表与外部扫描器由 [`yolowsyntax`] 提供；来源和许可见 `THIRD-PARTY.md`。

#![forbid(unsafe_code)]

pub use yolowsyntax::*;
