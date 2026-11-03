//! 子命令的实现，按用途分组；`main.rs` 的 `run()` 只负责分派。

pub(crate) mod assets;
pub(crate) mod extract;
pub(crate) mod pages;
pub(crate) mod service;
pub(crate) mod warc_in;
