//! 指纹档案：一个具体浏览器版本发什么。要升级版本：在 `benchmarks/history/1.0/fingerprint/run.sh`
//! 录一次新版本，这里加一个档案，`FingerprintProfile::Chrome` 的别名指过去。
//!
//! 数值全部来自实录（Chrome 156.0.8078.79，Linux，`results/` 里的记录），不是推断的。

/// 选用哪个浏览器的指纹。类型上和会话池的档案绑定（`pagesift_policy::session_pool::Profile`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FingerprintProfile {
    /// Chrome 156，Linux。
    Chrome156,
}

impl FingerprintProfile {
    /// 按配置里的名字找档案。`chrome` 指当前最新录制的版本。
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "chrome" | "chrome_156" => Some(Self::Chrome156),
            _ => None,
        }
    }

    /// 写进配置和元数据的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chrome156 => "chrome_156",
        }
    }

    /// 这个档案在线上发的 User-Agent。
    pub fn user_agent(self) -> &'static str {
        self.data().user_agent
    }

    pub(crate) fn data(self) -> &'static ProfileData {
        match self {
            Self::Chrome156 => &CHROME_156,
        }
    }
}

impl std::fmt::Display for FingerprintProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(crate) struct Http2Settings {
    pub header_table_size: u32,
    pub initial_stream_window: u32,
    pub max_header_list_size: u32,
    /// 连接级窗口的目标值。线上的 WINDOW_UPDATE 增量是它减去协议初始值 65535。
    pub connection_window: u32,
    /// HEADERS 帧的优先级字段：依赖流 0（exclusive 位为 1），权重字节。
    pub priority_dependency: u32,
    pub priority_weight: u8,
}

pub(crate) struct ProfileData {
    pub user_agent: &'static str,
    /// 不含 GREASE。首位的 GREASE 由 `hello.rs` 每连接生成。
    pub cipher_suites: &'static [u16],
    pub supported_groups: &'static [u16],
    pub signature_algorithms: &'static [u16],
    pub supported_versions: &'static [u16],
    pub alpn: &'static [u8],
    pub compress_certificate: &'static [u8],
    pub application_settings: &'static [u8],
    pub trust_anchors: &'static [u8],
    pub http2: Http2Settings,
    /// 导航请求的头，顺序就是 HTTP/2 线上的顺序。`user-agent` 也在里面。
    pub request_headers: &'static [(&'static str, &'static str)],
    /// HTTP/2 独有、排在最后的头。
    pub http2_only_headers: &'static [(&'static str, &'static str)],
}

/// [协议] 实录：Chrome 156 的 0xca34（Trust Anchor IDs）内容，是 Chrome 根库的一份静态 OID 列表。
const CHROME_156_TRUST_ANCHORS: &[u8] = &[
    0x00, 0xb8, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x01, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x06, 0x05, 0x82,
    0xdf, 0x13, 0x02, 0x0d, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x0e, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x0f,
    0x05, 0x82, 0xdf, 0x13, 0x02, 0x12, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x13, 0x05, 0x82, 0xdf, 0x13,
    0x02, 0x14, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x07, 0x08, 0x83, 0x9a, 0x64, 0x8c,
    0x9b, 0x2d, 0x01, 0x08, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x09, 0x08, 0x83, 0x9a,
    0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0a, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0b, 0x08,
    0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0c, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01,
    0x0d, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x12, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b,
    0x2d, 0x01, 0x13, 0x04, 0xd6, 0x79, 0x09, 0x01, 0x04, 0xd6, 0x79, 0x09, 0x04, 0x04, 0xd6, 0x79,
    0x09, 0x05, 0x04, 0xd6, 0x79, 0x09, 0x06, 0x04, 0xd6, 0x79, 0x09, 0x07, 0x04, 0xd6, 0x79, 0x09,
    0x08, 0x04, 0xd6, 0x79, 0x09, 0x0a, 0x04, 0xd6, 0x79, 0x09, 0x0b, 0x04, 0xd6, 0x79, 0x09, 0x0c,
    0x04, 0xd6, 0x79, 0x09, 0x0d, 0x04, 0xd6, 0x79, 0x09, 0x0f,
];

/// [协议] Chrome 156 实录。0xc013、0xc014、0x009c、0x009d、0x002f、0x0035 只声明，ring 和 rustls 没有实现。
const CHROME_156_SUITES: &[u16] = &[
    0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc013, 0xc014, 0x009c,
    0x009d, 0x002f, 0x0035,
];
/// [协议] Chrome 156 实录。
const CHROME_156_GROUPS: &[u16] = &[0x11ec, 0x001d, 0x0017, 0x0018];
/// [协议] Chrome 156 实录。0x0904–0x0906（ML-DSA）只声明：webpki 没有，服务器真用就握手失败。
const CHROME_156_SIGNATURES: &[u16] = &[
    0x0904, 0x0905, 0x0906, 0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601,
];
/// [协议] Chrome 156 实录。
const CHROME_156_VERSIONS: &[u16] = &[0x0304, 0x0303];
/// [协议] Chrome 156 实录：`h2`、`http/1.1`。
const CHROME_156_ALPN: &[u8] = b"\x00\x0c\x02h2\x08http/1.1";
/// [协议] Chrome 156 实录：只有 brotli。
const CHROME_156_COMPRESS_CERTIFICATE: &[u8] = &[0x02, 0x00, 0x02];
/// [协议] Chrome 156 实录：`h2`。
const CHROME_156_ALPS: &[u8] = b"\x00\x03\x02h2";

/// Chrome 156 的 ClientHello 和 HTTP/2 连接参数（见模块注释的结果文件）。服务器选了只声明的算法时的处理见 [`super`]。
static CHROME_156: ProfileData = ProfileData {
    user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36",
    cipher_suites: CHROME_156_SUITES,
    supported_groups: CHROME_156_GROUPS,
    signature_algorithms: CHROME_156_SIGNATURES,
    supported_versions: CHROME_156_VERSIONS,
    alpn: CHROME_156_ALPN,
    compress_certificate: CHROME_156_COMPRESS_CERTIFICATE,
    application_settings: CHROME_156_ALPS,
    trust_anchors: CHROME_156_TRUST_ANCHORS,
    http2: Http2Settings {
        header_table_size: 65536,         // [协议] Chrome 156 实录
        initial_stream_window: 6291456,   // [协议] Chrome 156 实录
        max_header_list_size: 262144,     // [协议] Chrome 156 实录
        connection_window: 15728640,      // [协议] Chrome 156 实录
        priority_dependency: 0x8000_0000, // [协议] Chrome 156 实录
        priority_weight: 255,             // [协议] Chrome 156 实录
    },
    request_headers: &[
        (
            "sec-ch-ua",
            "\"Not:A-Brand\";v=\"8\", \"Chromium\";v=\"156\", \"Google Chrome\";v=\"156\"",
        ),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"Linux\""),
        ("upgrade-insecure-requests", "1"),
        (
            "user-agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36",
        ),
        (
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/jxl,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
        ),
        ("sec-fetch-site", "none"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-user", "?1"),
        ("sec-fetch-dest", "document"),
        ("accept-encoding", "gzip, deflate, br, zstd"),
        ("accept-language", "en-US,en;q=0.9"),
    ],
    http2_only_headers: &[("priority", "u=0, i")],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(
            FingerprintProfile::parse("chrome"),
            Some(FingerprintProfile::Chrome156)
        );
        assert_eq!(
            FingerprintProfile::parse("chrome_156"),
            Some(FingerprintProfile::Chrome156)
        );
        assert_eq!(FingerprintProfile::parse("firefox"), None);
        assert_eq!(FingerprintProfile::Chrome156.to_string(), "chrome_156");
    }

    /// UA 写了两份（`user_agent` 给调用方看，`request_headers` 发上线），不能不一致。
    #[test]
    fn user_agent_matches_headers() {
        let data = FingerprintProfile::Chrome156.data();
        let wire = data
            .request_headers
            .iter()
            .find(|(name, _)| *name == "user-agent");
        assert_eq!(wire.map(|(_, value)| *value), Some(data.user_agent));
    }
}
