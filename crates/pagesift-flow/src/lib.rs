//! 数据面与执行面。
//!
//! 0.84：CPU 工人池（[`cpu`]）与字节嗅探（[`sniff`]）。正文提取、PDF/Office 解析、解压转码、缓存编解码都在工人上执行，
//! 不占异步线程。
//!
//! 0.86：统一准入门面（[`admit`]）：CPU、网络在途、浏览器标签、内存四种资源的用量、上限与影子价格。

pub mod admit;
pub mod cpu;
pub mod sniff;

pub use admit::{Admission, Snapshot, TabPermit};
pub use cpu::{CpuSet, Panicked, Pool};
