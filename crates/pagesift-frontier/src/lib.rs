//! 爬取的前沿和聚焦打分：去重、URL 陷阱与模板降级（[`frontier`]），带查询时的链接打分、
//! 相关度优先队列和自适应停止（[`focus`]）。不发请求、不碰引擎，由 `pagesift` 的爬取循环调用。

#![forbid(unsafe_code)]

pub mod focus;
pub mod frontier;
