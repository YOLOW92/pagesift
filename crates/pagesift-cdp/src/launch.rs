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

pub(crate) use crate::endpoint::resolve_endpoint;
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

/// 找名为 `names` 之一的可执行文件：先按 `PATH` 的顺序，再看 `PATH` 之外常见的安装目录
/// （见 [`command_dirs`]）。
pub(crate) fn find_command(names: &[&str]) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_command_in(names, std::env::var_os("PATH"), home.as_deref())
}

fn find_command_in(
    names: &[&str],
    path: Option<std::ffi::OsString>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    command_dirs(path, home)
        .into_iter()
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| is_executable(p))
}

/// 找可执行文件的目录：`PATH` 里的，然后是 `~/.local/bin`、`~/bin` 和系统的 bin 目录。
/// 后面这些不一定在 `PATH` 里：`sudo` 换成 secure_path、systemd 和 cron 只给精简的 `PATH`，
/// 用户装在自己目录里的内核就"明明在却找不到"。重复的目录只看一次。
fn command_dirs(path: Option<std::ffi::OsString>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join("bin"));
    }
    for d in ["/usr/local/bin", "/usr/bin", "/opt/homebrew/bin"] {
        dirs.push(PathBuf::from(d));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
    dirs
}

/// Playwright 下载的浏览器（`npx playwright install`，做 Agent 的机器上很常见）：
/// `$PLAYWRIGHT_BROWSERS_PATH` 或默认缓存目录里各版本的 Chromium，新版本在前。
fn playwright_browsers(cache: &Path) -> Vec<PathBuf> {
    // 版本目录名形如 chromium-1243、chromium_headless_shell-1243
    let version = |name: &str| {
        name.rsplit('-')
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let mut versions: Vec<(u32, PathBuf)> = std::fs::read_dir(cache)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.starts_with("chromium")
                .then(|| (version(&name), e.path()))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    const EXECUTABLES: &[&str] = &[
        "chrome-headless-shell",
        "headless_shell",
        "chrome",
        "Chromium.app/Contents/MacOS/Chromium",
        "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    ];
    versions
        .into_iter()
        .flat_map(|(_, dir)| {
            let mut subdirs: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .collect();
            subdirs.sort();
            subdirs
        })
        .flat_map(|sub| EXECUTABLES.iter().map(move |n| sub.join(n)))
        .filter(|p| is_executable(p))
        .collect()
}

fn playwright_cache(home: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let home = home?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/ms-playwright")
    } else {
        home.join(".cache/ms-playwright")
    })
}

/// 在 `PATH`、常见安装位置和 Playwright 的浏览器缓存里找 Chrome/Chromium。
pub fn detect_browser() -> Option<PathBuf> {
    if let Some(p) = find_command(PATH_NAMES) {
        return Some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = ABSOLUTE_PATHS.iter().map(PathBuf::from).collect();
    if cfg!(target_os = "macos")
        && let Some(home) = &home
    {
        for p in ABSOLUTE_PATHS {
            candidates.push(home.join(p.trim_start_matches('/')));
        }
    }
    candidates
        .into_iter()
        .find(|p| is_executable(p))
        .or_else(|| {
            playwright_cache(home.as_deref())
                .and_then(|c| playwright_browsers(&c).into_iter().next())
        })
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

/// 当前进程是否以 root（euid 0）运行。root 下无法启动 Chrome 沙箱，doctor 与报错文案
/// 用它把“装没装引擎”和“被启动策略拒绝”区分开。
pub fn running_as_root() -> bool {
    nix::unistd::geteuid().is_root()
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
        // 上一个进程被强杀时留下的浏览器，先收掉
        crate::sweep::sweep_once();
        let mut args = spec.args_override.clone().unwrap_or_else(default_args);
        args.extend(spec.extra_args.iter().cloned());
        check_args(&args)?;

        let profile = tempfile::Builder::new()
            .prefix(crate::sweep::PREFIX)
            .tempdir()?;
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
        // 浏览器已经起来（命令行是它自己的，不是 fork 出来还没 exec 的副本），记下来供下次回收
        crate::sweep::record(process._profile.path(), process.pid());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn commands_are_found_in_path_then_in_the_users_bin_dirs() {
        let t = tempfile::tempdir().unwrap();
        let (custom, home) = (t.path().join("custom"), t.path().join("home"));
        exe(&custom.join("lightpanda"));
        exe(&home.join(".local/bin/moli"));
        let path = Some(std::ffi::OsString::from(custom.as_os_str()));
        // PATH 里的目录（任意位置，比如版本管理器的）
        assert_eq!(
            find_command_in(&["lightpanda"], path.clone(), Some(&home)),
            Some(custom.join("lightpanda"))
        );
        // ~/.local/bin 不在 PATH 里（sudo、systemd、cron 的精简 PATH）也找得到
        assert_eq!(
            find_command_in(&["moli"], None, Some(&home)),
            Some(home.join(".local/bin/moli"))
        );
        // 不可执行的文件不算
        std::fs::write(custom.join("chromium"), b"x").unwrap();
        assert_eq!(find_command_in(&["chromium"], path, Some(&home)), None);
    }

    #[test]
    fn the_newest_playwright_chromium_is_found() {
        let t = tempfile::tempdir().unwrap();
        exe(&t.path().join("chromium-1100/chrome-linux/chrome"));
        exe(&t.path().join("chromium-1243/chrome-linux64/chrome"));
        exe(&t
            .path()
            .join("chromium_headless_shell-1243/chrome-linux/headless_shell"));
        std::fs::create_dir_all(t.path().join("ffmpeg-1011")).unwrap();
        let found = playwright_browsers(t.path());
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].to_string_lossy().contains("1243"), "{found:?}");
        assert!(
            found[2].ends_with("chromium-1100/chrome-linux/chrome"),
            "{found:?}"
        );
    }

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
