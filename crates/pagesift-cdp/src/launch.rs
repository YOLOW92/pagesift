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
        let args: Vec<String> = args
            .iter()
            .map(|a| a.replace(USER_DATA_DIR, &dir))
            .collect();

        let mut cmd = Command::new(&spec.executable);
        cmd.args(&args)
            .arg("about:blank")
            .stdin(Stdio::null())
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
        let found = tokio::time::timeout(spec.timeout, async {
            while let Some(line) = lines.recv().await {
                if let Some(url) = find_ws_url(&line) {
                    return Some(url);
                }
                if tail.len() == 8 {
                    tail.remove(0);
                }
                tail.push(line);
            }
            None
        })
        .await;
        match found {
            Ok(Some(url)) => process.ws_url = url,
            Ok(None) => {
                process.shutdown().await;
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
