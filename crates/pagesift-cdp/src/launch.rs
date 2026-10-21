//! 启动和清理浏览器进程，以及远程端点解析。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::error::{Error, Result};

const PATH_NAMES: &[&str] = &[
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
    "chrome",
    "microsoft-edge",
    "microsoft-edge-stable",
];

#[cfg(target_os = "macos")]
const ABSOLUTE_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

#[cfg(not(target_os = "macos"))]
const ABSOLUTE_PATHS: &[&str] = &[
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/usr/bin/google-chrome",
    "/snap/bin/chromium",
    "/opt/google/chrome/chrome",
];

/// 在 `PATH` 和常见安装位置里找 Chrome/Chromium。
pub fn detect_browser() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in PATH_NAMES {
                let p = dir.join(name);
                if is_executable(&p) {
                    return Some(p);
                }
            }
        }
    }
    let mut candidates: Vec<PathBuf> = ABSOLUTE_PATHS.iter().map(PathBuf::from).collect();
    if cfg!(target_os = "macos")
        && let Some(home) = std::env::var_os("HOME")
    {
        let home = PathBuf::from(home);
        for p in ABSOLUTE_PATHS {
            candidates.push(home.join(p.trim_start_matches('/')));
        }
    }
    candidates.into_iter().find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub(crate) fn ensure_not_root() -> Result<()> {
    if nix::unistd::geteuid().is_root() {
        return Err(Error::RunningAsRoot);
    }
    Ok(())
}

/// 关闭沙箱、关闭同源策略、伪装 UA、把调试端口暴露到外网的参数一律拒绝。
const FORBIDDEN_ARGS: &[&str] = &[
    "no-sandbox",
    "disable-setuid-sandbox",
    "disable-gpu-sandbox",
    "disable-seccomp-filter-sandbox",
    "disable-namespace-sandbox",
    "no-zygote-sandbox",
    "disable-web-security",
    "user-agent",
    "remote-debugging-address",
];

pub(crate) fn check_args(args: &[String]) -> Result<()> {
    for arg in args {
        let key = arg
            .trim_start_matches('-')
            .split('=')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if FORBIDDEN_ARGS.contains(&key.as_str()) {
            return Err(Error::ForbiddenArgument(arg.clone()));
        }
    }
    Ok(())
}

pub(crate) const USER_DATA_DIR: &str = "{user_data_dir}";
/// 参数里的 `{port}` 会换成启动前挑好的空闲端口。命令行和 Chrome 不同的浏览器（Lightpanda、
/// obscura、moli）用它指定 CDP 端口，启动后从标准的 `/json/version` 读调试地址。
pub(crate) const PORT: &str = "{port}";

fn default_args() -> Vec<String> {
    [
        "--headless=new",
        "--remote-debugging-port=0",
        "--user-data-dir={user_data_dir}",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--disable-extensions",
        "--disable-default-apps",
        "--disable-breakpad",
        "--disable-crash-reporter",
        "--disable-background-timer-throttling",
        "--disable-renderer-backgrounding",
        "--disable-backgrounding-occluded-windows",
        "--disable-features=Translate,MediaRouter,OptimizationHints",
        "--password-store=basic",
        "--use-mock-keychain",
        "--mute-audio",
        "--hide-scrollbars",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct LaunchSpec {
    pub executable: PathBuf,
    /// 替换默认参数（给 obscura、moli 这类命令行不同的浏览器用）。
    pub args_override: Option<Vec<String>>,
    pub extra_args: Vec<String>,
    pub timeout: Duration,
}

/// 一个浏览器子进程。它在自己的进程组里，清理时整组 SIGKILL。
pub(crate) struct BrowserProcess {
    child: Child,
    pgid: Pid,
    ws_url: String,
    killed: bool,
    _profile: tempfile::TempDir,
}

impl BrowserProcess {
    pub async fn launch(spec: &LaunchSpec) -> Result<BrowserProcess> {
        ensure_not_root()?;
        let mut args = spec.args_override.clone().unwrap_or_else(default_args);
        args.extend(spec.extra_args.iter().cloned());
        check_args(&args)?;

        let profile = tempfile::Builder::new().prefix("pagesift-cdp-").tempdir()?;
        let dir = profile.path().to_string_lossy().into_owned();
        let port = if args.iter().any(|a| a.contains(PORT)) {
            Some(free_port()?)
        } else {
            None
        };
        let args: Vec<String> = args
            .iter()
            .map(|a| {
                let a = a.replace(USER_DATA_DIR, &dir);
                match port {
                    Some(p) => a.replace(PORT, &p.to_string()),
                    None => a,
                }
            })
            .collect();

        // Chrome 每次启动都会在 TMPDIR 里建临时目录，正常退出时才删；被强杀的话就留在 /tmp。
        // 指到配置目录里面，跟配置目录一起清掉，长期运行的服务 /tmp 不会越积越多。
        let tmp = profile.path().join("tmp");
        std::fs::create_dir(&tmp)?;
        let mut cmd = Command::new(&spec.executable);
        cmd.env("TMPDIR", &tmp).args(&args);
        // Chrome 需要一个初始页；自定义参数的浏览器（Lightpanda 等）不认这个参数
        if spec.args_override.is_none() {
            cmd.arg("about:blank");
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Launch(format!("{}: {e}", spec.executable.display())))?;
        let (tx, mut lines) = mpsc::unbounded_channel();
        if let Some(out) = child.stdout.take() {
            forward_lines(out, tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            forward_lines(err, tx);
        }
        let Some(pid) = child.id() else {
            return Err(Error::Launch("process exited immediately".into()));
        };
        // 从这里开始，任何一步失败都会经 Drop 清理整个进程组。
        let mut process = BrowserProcess {
            pgid: Pid::from_raw(pid as i32),
            child,
            ws_url: String::new(),
            killed: false,
            _profile: profile,
        };

        let mut tail: Vec<String> = Vec::new();
        let mut sandbox_line: Option<String> = None;
        let found = tokio::time::timeout(spec.timeout, async {
            // 指定了端口的浏览器：轮询 /json/version，和读输出同时进行，哪个先拿到地址用哪个
            let probe = async {
                let Some(port) = port else {
                    return std::future::pending::<Option<String>>().await;
                };
                loop {
                    if let Some(url) = json_version_ws_url(port).await {
                        return Some(url);
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            };
            tokio::pin!(probe);
            loop {
                let line = tokio::select! {
                    url = &mut probe => return url,
                    line = lines.recv() => line,
                };
                let Some(line) = line else { break };
                if let Some(url) = find_ws_url(&line) {
                    return Some(url);
                }
                if sandbox_line.is_none() && line.to_ascii_lowercase().contains("sandbox") {
                    sandbox_line = Some(line.clone());
                }
                if tail.len() == 8 {
                    tail.remove(0);
                }
                tail.push(line);
            }
            None
        })
        .await;
        // 读完输出（进程退出）也可能是因为浏览器把日志写进了文件；端口模式再给探测一次机会
        let found = match (found, port) {
            (Ok(None), Some(p)) => Ok(json_version_ws_url(p).await),
            (f, _) => f,
        };
        match found {
            Ok(Some(url)) => process.ws_url = url,
            Ok(None) => {
                process.shutdown().await;
                if let Some(line) = sandbox_line {
                    return Err(Error::Launch(format!(
                        "the browser sandbox is unavailable ({}). On Ubuntu 23.10+ AppArmor blocks \
                         unprivileged user namespaces: allow them with \
                         `sysctl kernel.apparmor_restrict_unprivileged_userns=0` or install an \
                         AppArmor profile for the browser. pagesift never runs browsers with --no-sandbox",
                        line.trim()
                    )));
                }
                return Err(Error::Launch(format!(
                    "browser exited: {}",
                    tail.join(" | ")
                )));
            }
            Err(_) => {
                process.shutdown().await;
                return Err(Error::Launch(
                    "timed out waiting for the DevTools endpoint".into(),
                ));
            }
        }
        tokio::spawn(async move {
            while let Some(line) = lines.recv().await {
                tracing::trace!(target: "pagesift_cdp::browser", "{line}");
            }
        });
        Ok(process)
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    pub fn pid(&self) -> u32 {
        self.pgid.as_raw() as u32
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn kill_group(&mut self) {
        if !self.killed {
            self.killed = true;
            let _ = killpg(self.pgid, Signal::SIGKILL);
        }
    }

    /// 整组 SIGKILL，然后回收主进程，保证不留僵尸。
    pub async fn shutdown(mut self) {
        self.kill_group();
        let _ = self.child.wait().await;
    }
}

impl Drop for BrowserProcess {
    fn drop(&mut self) {
        // 来不及 await 时的兜底；主进程由 tokio 的孤儿回收负责 wait。
        self.kill_group();
    }
}

fn forward_lines<R: AsyncRead + Unpin + Send + 'static>(r: R, tx: mpsc::UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// 挑一个本机空闲端口（绑定 0 端口再放开）。
fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// 从 CDP 标准的 `/json/version` 读浏览器级 WebSocket 地址；服务还没起来时返回 `None`。
async fn json_version_ws_url(port: u16) -> Option<String> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    let req = format!(
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.ok()?;
    // 有的实现回完不关连接：读到完整的头和 Content-Length 那么长的正文就停
    let mut buf = Vec::new();
    let body = tokio::time::timeout(Duration::from_secs(2), async {
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).await.ok()?;
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok());
                let body = &buf[end + 4..];
                if n == 0 || len.is_some_and(|l| body.len() >= l) {
                    return Some(body.to_vec());
                }
            } else if n == 0 {
                return None;
            }
        }
    })
    .await
    .ok()??;
    let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let url = v.get("webSocketDebuggerUrl")?.as_str()?;
    // 有的实现写 localhost；统一连回 127.0.0.1，免得解析成 ::1 连不上
    Some(url.replacen("ws://localhost:", "ws://127.0.0.1:", 1))
}

fn find_ws_url(line: &str) -> Option<String> {
    let start = line.find("ws://")?;
    let url = line[start..].split_whitespace().next()?;
    Some(url.to_owned())
}

/// 远程端点：`ws://` 直接用；`http://host:port` 读 `/json/version` 拿 WebSocket 地址。
pub(crate) async fn resolve_endpoint(endpoint: &str) -> Result<String> {
    let url = url::Url::parse(endpoint).map_err(|e| Error::Connect(format!("{endpoint}: {e}")))?;
    match url.scheme() {
        "ws" => return Ok(endpoint.to_owned()),
        "http" => {}
        s => {
            return Err(Error::Connect(format!(
                "unsupported CDP endpoint scheme `{s}` (use ws:// or http://)"
            )));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Connect("endpoint has no host".into()))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let fetch = async {
        let mut stream = tokio::net::TcpStream::connect((host, port)).await?;
        let req = format!(
            "GET /json/version HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        // DevTools 的 HTTP 服务不一定按 Connection: close 断开，按 Content-Length 读。
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = stream.read(&mut chunk).await?;
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.len() > 1 << 20 || http_body_complete(&buf) {
                break;
            }
        }
        Ok::<_, std::io::Error>(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(10), fetch)
        .await
        .map_err(|_| Error::Connect("timed out reading /json/version".into()))?
        .map_err(|e| Error::Connect(e.to_string()))?;
    let text = String::from_utf8_lossy(&buf);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|e| Error::Connect(format!("bad /json/version: {e}")))?;
    let ws = v
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connect("no webSocketDebuggerUrl in /json/version".into()))?;
    // 浏览器可能报出 localhost 之类的地址，换成调用方给的主机和端口。
    let mut ws = url::Url::parse(ws).map_err(|e| Error::Connect(e.to_string()))?;
    let _ = ws.set_host(Some(host));
    let _ = ws.set_port(Some(port));
    Ok(ws.to_string())
}

fn http_body_complete(buf: &[u8]) -> bool {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    len.is_some_and(|len| buf.len() >= end + 4 + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_sandbox_and_ua_flags() {
        for bad in [
            "--no-sandbox",
            "--disable-setuid-sandbox",
            "--user-agent=Mozilla/5.0",
            "--NO-SANDBOX",
            "--remote-debugging-address=0.0.0.0",
        ] {
            assert!(
                matches!(
                    check_args(&[bad.to_string()]),
                    Err(Error::ForbiddenArgument(_))
                ),
                "{bad}"
            );
        }
        assert!(check_args(&default_args()).is_ok());
        assert!(check_args(&["--window-size=800,600".into()]).is_ok());
    }

    #[test]
    fn parses_devtools_line() {
        let line = "DevTools listening on ws://127.0.0.1:41235/devtools/browser/abc-def";
        assert_eq!(
            find_ws_url(line).as_deref(),
            Some("ws://127.0.0.1:41235/devtools/browser/abc-def")
        );
        assert_eq!(find_ws_url("[1021/1:ERROR] something"), None);
    }
}
