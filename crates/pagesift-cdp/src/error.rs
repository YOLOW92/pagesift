use std::fmt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// 没有配置浏览器路径，也没有在常见位置找到浏览器。
    NoBrowserFound,
    /// 当前用户是 root。Chrome 的沙箱在 root 下无法工作，我们不会替你关掉沙箱。
    RunningAsRoot,
    /// 配置里出现了关闭沙箱、伪装 UA 之类不允许的启动参数。
    ForbiddenArgument(String),
    Launch(String),
    Connect(String),
    ConnectionClosed,
    /// 浏览器对某条 CDP 命令返回了错误。
    Protocol {
        method: String,
        code: i64,
        message: String,
    },
    UnexpectedResponse(String),
    Timeout,
    /// 导航失败（`net::ERR_*`）。
    Navigation(String),
    /// 主文档被 [`RequestGuard`](crate::RequestGuard) 拒绝。
    Blocked {
        url: String,
        reason: String,
    },
    /// 某个子 target 无法开启请求拦截，为了不漏检，本次渲染失败。
    InterceptionUnavailable(String),
    InvalidOptions(String),
    /// 请求了 `executeJavascript`，但配置里没有开启。
    ExecuteJavascriptDisabled,
    /// 第 `index` 个 action 执行失败。
    Action {
        index: usize,
        message: String,
    },
    PoolClosed,
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoBrowserFound => f.write_str("no Chrome/Chromium executable found; set the browser path in the configuration"),
            Error::RunningAsRoot => f.write_str("refusing to launch the browser as root: the Chrome sandbox does not work as root and pagesift will not disable it; run as an unprivileged user"),
            Error::ForbiddenArgument(a) => write!(f, "browser argument not allowed: {a}"),
            Error::Launch(m) => write!(f, "failed to launch browser: {m}"),
            Error::Connect(m) => write!(f, "failed to connect to CDP endpoint: {m}"),
            Error::ConnectionClosed => f.write_str("CDP connection closed"),
            Error::Protocol { method, code, message } => write!(f, "CDP error in {method} ({code}): {message}"),
            Error::UnexpectedResponse(m) => write!(f, "unexpected CDP response: {m}"),
            Error::Timeout => f.write_str("render timed out"),
            Error::Navigation(m) => write!(f, "navigation failed: {m}"),
            Error::Blocked { url, reason } => write!(f, "request to {url} blocked: {reason}"),
            Error::InterceptionUnavailable(m) => write!(f, "request interception unavailable: {m}"),
            Error::InvalidOptions(m) => write!(f, "invalid render options: {m}"),
            Error::ExecuteJavascriptDisabled => f.write_str("the executeJavascript action is disabled in the configuration"),
            Error::Action { index, message } => write!(f, "action #{index} failed: {message}"),
            Error::PoolClosed => f.write_str("browser pool is shut down"),
            Error::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
