//! pagesift 的集成测试合成一个测试程序：每个模块原来是 tests/ 下单独的文件，各自编译、各自链接一遍
//! pagesift；合成一个后只链接一次。`dom_once` 数的是进程内的 DOM 构建次数，和别的测试同进程会数乱，
//! 所以它仍是单独的测试程序。

#[path = "../../../pagesift-net/tests/common/mod.rs"]
mod common;

mod ai_content;
mod cli_fetch;
mod cli_pipe;
mod cli_signals;
mod cluster;
mod collect;
mod crawl;
mod download;
mod extract;
mod focused;
mod focused_site;
mod frontier;
mod images;
mod multihost;
mod office;
mod parse;
mod scrape;
mod scripts;
mod serve;
mod session;
mod session_cli;
mod seven_cli;
mod warc_cli;
