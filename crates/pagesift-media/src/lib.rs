//! 媒体：HLS/DASH 流下载（[`stream`]）、媒体条目（[`media`]）、可续传下载（[`download`]）
//! 和下载后的发布（[`postprocess`]）。

#![forbid(unsafe_code)]

pub mod download;
pub mod media;
pub mod postprocess;
pub mod stream;
