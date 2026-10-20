//! MCP 协议常量：版本、错误码、`_meta` 键。

/// 当前版本（无状态，每个请求在 `_meta` 里带版本号）。
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";

/// 支持的新版（无状态）协议版本。
pub const MODERN_VERSIONS: &[&str] = &["2026-07-28"];

/// 支持的旧版（`initialize` 握手）协议版本，按新到旧排列。
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// 允许 JSON-RPC 批量请求的协议版本（2025-06-18 起取消了批量）。
pub const BATCH_VERSIONS: &[&str] = &["2025-03-26"];

pub const HEADER_MISMATCH: i32 = -32020;
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i32 = -32021;
pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// 所有支持的版本，新版在前。
pub fn supported_versions() -> Vec<&'static str> {
    MODERN_VERSIONS
        .iter()
        .chain(LEGACY_VERSIONS)
        .copied()
        .collect()
}

pub(crate) fn find_version(list: &[&'static str], v: &str) -> Option<&'static str> {
    list.iter().copied().find(|x| *x == v)
}

/// 客户端和服务器之间采用的协议代际。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// 2026-07-28 起：无状态，元数据随请求携带。
    Modern(&'static str),
    /// 2025-11-25 及更早：先 `initialize` 握手。
    Legacy(&'static str),
}

impl Era {
    pub fn version(self) -> &'static str {
        match self {
            Era::Modern(v) | Era::Legacy(v) => v,
        }
    }
}
