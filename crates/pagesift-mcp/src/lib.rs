//! pagesift 的 MCP 服务器：自研 JSON-RPC 2.0 和 MCP 协议，支持 stdio 和 Streamable HTTP。
//!
//! # 协议版本
//!
//! 实现的是 MCP **2026-07-28**（当前版本，无状态：每个请求在 `_meta` 里带
//! `io.modelcontextprotocol/protocolVersion` 和 `clientCapabilities`，服务器实现
//! `server/discover`）。同时兼容走 `initialize` 握手的旧版本 2025-11-25、2025-06-18、
//! 2025-03-26 和 2024-11-05，按规范的"双代"服务器行为：带新版 `_meta` 的请求无状态处理，
//! `initialize` 选择旧版语义（stdio 下作用于整个进程，HTTP 下作用于会话）。
//! JSON-RPC 批量请求只在协商到 2025-03-26 时接受（之后的版本取消了批量）。
//!
//! # 用法
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use pagesift_mcp::{McpServer, tools};
//!
//! let server = McpServer::builder("pagesift", env!("CARGO_PKG_VERSION"))
//!     .tools(tools::placeholder_tools())
//!     .build()
//!     .expect("valid tools");
//! pagesift_mcp::serve_stdio(server).await
//! # }
//! ```
//!
//! 真正的抓取由上层实现 [`ToolHandler`]，定义沿用 [`tools`] 里对齐 firecrawl-mcp 的 schema。

#![forbid(unsafe_code)]

pub mod http;
pub mod jsonrpc;
pub mod protocol;
pub mod schema;
mod server;
mod stdio;
mod tool;
pub mod tools;

pub use http::{HttpConfig, router, serve_http, serve_listener};
pub use protocol::{Era, LATEST_PROTOCOL_VERSION};
pub use server::{BuildError, McpServer, ServerBuilder};
pub use stdio::{MAX_LINE_BYTES, serve_stdio, serve_stream};
pub use tool::{
    BoxFuture, Content, ToolAnnotations, ToolContext, ToolDef, ToolError, ToolHandler, ToolOutput,
    ToolResult,
};
