//! 只带占位工具的 MCP 服务器，用于联调和端到端测试。
//!
//! `pagesift-mcp`（stdio）或 `pagesift-mcp --http [地址]`。

#![forbid(unsafe_code)]

use std::process::ExitCode;

use pagesift_mcp::{HttpConfig, McpServer, tools};

#[tokio::main]
async fn main() -> ExitCode {
    let server = match McpServer::builder("pagesift", env!("CARGO_PKG_VERSION"))
        .title("pagesift (placeholder tools)")
        .tools(tools::placeholder_tools())
        .build()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pagesift-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("--stdio") => pagesift_mcp::serve_stdio(server).await,
        Some("--http") => {
            let mut config = HttpConfig::default();
            if let Some(addr) = args.get(1) {
                match addr.parse() {
                    Ok(a) => config.bind = a,
                    Err(e) => {
                        eprintln!("pagesift-mcp: bad address {addr:?}: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            let listener = match tokio::net::TcpListener::bind(config.bind).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("pagesift-mcp: cannot bind {}: {e}", config.bind);
                    return ExitCode::FAILURE;
                }
            };
            if let Ok(a) = listener.local_addr() {
                eprintln!("pagesift-mcp listening on http://{a}{}", config.path);
            }
            pagesift_mcp::serve_listener(server, &config, listener, std::future::pending()).await
        }
        Some(other) => {
            eprintln!("usage: pagesift-mcp [--stdio | --http [ADDR]] (unknown argument {other:?})");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pagesift-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}
