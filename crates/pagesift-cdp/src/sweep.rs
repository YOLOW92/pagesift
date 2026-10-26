//! 回收上一个进程留下的浏览器。
//!
//! 浏览器在自己的进程组里运行，正常退出或 SIGTERM 时由 `BrowserProcess` 整组结束。
//! 属主进程被 SIGKILL、被内存不足杀掉或崩溃时来不及清理，浏览器和它的配置目录就会一直留着
//! （一次压测留下几十个无头 Chrome，占满内存）。本 crate 不用 unsafe，没法设 PR_SET_PDEATHSIG，
//! 所以改为：启动浏览器后在配置目录里记下属主进程号、浏览器进程号和浏览器的命令行；
//! 每个进程第一次启动浏览器前扫一遍临时目录，属主已经不在的，核对命令行一致后整组结束，再删目录。
//! 核对命令行是为了防止进程号被系统重新分配后误杀别的进程；读不到命令行（非 Linux）就只删目录、不结束进程。

use std::path::Path;
use std::sync::Once;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;

/// 配置目录里的记录文件名。
const OWNER_FILE: &str = "pagesift-owner";
/// 配置目录的前缀（和 `tempfile::Builder::prefix` 一致）。
pub(crate) const PREFIX: &str = "pagesift-cdp-";

/// 浏览器启动成功后调用：记下属主（本进程）、浏览器进程号和它的命令行。
pub(crate) fn record(profile: &Path, browser: u32) {
    let mut data = format!("{}\n{}\n", std::process::id(), browser).into_bytes();
    if let Some(cmd) = cmdline(browser) {
        data.extend_from_slice(&cmd);
    }
    let _ = std::fs::write(profile.join(OWNER_FILE), data);
}

/// 本进程第一次启动浏览器前调用一次。
pub(crate) fn sweep_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| sweep(&std::env::temp_dir()));
}

pub(crate) fn sweep(tmp: &Path) {
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return;
    };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with(PREFIX) {
            continue;
        }
        let dir = e.path();
        // 没有记录文件：可能是别的进程正在启动，也可能是旧版本留下的，都不碰
        let Ok(data) = std::fs::read(dir.join(OWNER_FILE)) else {
            continue;
        };
        if let Some(true) = reap(&data) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// 属主已经不在时结束浏览器；返回 Some(true) 表示目录可以删。
fn reap(data: &[u8]) -> Option<bool> {
    let mut parts = data.splitn(3, |&b| b == b'\n');
    let owner: i32 = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
    let browser: i32 = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
    let recorded = parts.next().unwrap_or_default();
    if owner <= 0 || browser <= 0 || alive(owner) {
        return Some(false);
    }
    if alive(browser) {
        match cmdline(browser as u32) {
            // 还是那个浏览器：整组结束
            Some(now) if !recorded.is_empty() && now == recorded => {
                let _ = killpg(Pid::from_raw(browser), Signal::SIGKILL);
            }
            // 进程号已被别的进程占用：浏览器早没了，只删目录
            Some(_) => {}
            // 读不到命令行（非 Linux）：没法确认，进程和目录都不碰
            None => return Some(false),
        }
    }
    Some(true)
}

fn alive(pid: i32) -> bool {
    !matches!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH))
}

fn cmdline(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .filter(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 属主还活着（本进程）：不动。
    #[test]
    fn live_owner_is_left_alone() {
        let data = format!("{}\n{}\n", std::process::id(), std::process::id());
        assert_eq!(reap(data.as_bytes()), Some(false));
    }

    /// 属主和浏览器都不在了：删目录。
    #[test]
    fn dead_owner_and_browser_dir_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(format!("{PREFIX}x"));
        std::fs::create_dir(&dir).unwrap();
        // 进程号上限之外的号一定不存在
        std::fs::write(dir.join(OWNER_FILE), "2147483000\n2147483001\n").unwrap();
        sweep(tmp.path());
        assert!(!dir.exists());
    }

    /// 没有记录文件的目录（正在启动、旧版本）：不动。
    #[test]
    fn dir_without_record_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(format!("{PREFIX}y"));
        std::fs::create_dir(&dir).unwrap();
        sweep(tmp.path());
        assert!(dir.exists());
    }

    /// 属主不在、浏览器进程号被别的进程占用（命令行不同）：不结束那个进程，只删目录。
    #[cfg(target_os = "linux")]
    #[test]
    fn reused_browser_pid_is_not_killed() {
        let data = format!("2147483000\n{}\nsomething-else\0", std::process::id());
        assert_eq!(reap(data.as_bytes()), Some(true));
        assert!(alive(std::process::id() as i32));
    }

    /// 属主被 SIGKILL 后留下的浏览器进程组被结束。
    #[cfg(target_os = "linux")]
    #[test]
    fn orphaned_browser_group_is_killed() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(format!("{PREFIX}z"));
        std::fs::create_dir(&dir).unwrap();
        // 刚 fork 出来、exec 还没完成时读到的是空的或父进程的命令行：等它变成 sleep
        // （生产代码在浏览器的 DevTools 端点起来之后才记录，不会碰到这个窗口）
        let cmd = (0..200)
            .find_map(|_| {
                let c = cmdline(pid).filter(|c| c.starts_with(b"sleep"));
                if c.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                c
            })
            .expect("sleep 的命令行");
        let mut data = format!("2147483000\n{pid}\n").into_bytes();
        data.extend(cmd);
        std::fs::write(dir.join(OWNER_FILE), data).unwrap();
        sweep(tmp.path());
        let status = child.wait().unwrap();
        assert!(!status.success(), "进程组应已被 SIGKILL 结束");
        assert!(!dir.exists());
    }
}
