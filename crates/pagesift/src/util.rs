//! 子命令共用的辅助函数：输出告警与退出码、运行时、输入读取与解码。

use crate::*;

/// 抓取输出层要显式告知的 warning：和 JSON 里的 `data.warning` 是同一份名单（[`pagesift::notice`]）。
pub(crate) fn print_scrape_warnings(doc: &pagesift::api::types::Document) {
    let Some(p) = doc.metadata.pagesift.as_ref() else {
        return;
    };
    for warning in p
        .warnings
        .iter()
        .filter(|w| pagesift::notice::is_content_risk(w))
    {
        eprintln!("warning: {warning}");
    }
}

/// 页面抓到了，但 HTTP 状态说明这不是一次成功的内容获取：5xx，404 或没跟随成功的 3xx 且正文为空。
/// 退出码 2 便于脚本区分“请求本身失败”（1）和“服务器返回错误页”（2）。
pub(crate) fn scrape_exit_code(doc: &pagesift::api::types::Document) -> ExitCode {
    let status = doc.metadata.status_code;
    let empty = doc.markdown.as_deref().is_none_or(|m| m.trim().is_empty());
    if status >= 500 || ((status == 404 || (300..400).contains(&status)) && empty) {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Ctrl-C，或者 SIGTERM（`docker stop`、systemd）。进行中的任务已经落盘，下次启动接着跑。
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutting down");
}

pub(crate) fn configured_policy(cfg: &pagesift::config::Config) -> Result<pagesift_policy::Policy> {
    let sc = cfg.scraper(false)?;
    Ok(pagesift_policy::Policy::new(
        Arc::new(pagesift_net::Fetcher::new(sc.fetcher)?),
        sc.policy,
    ))
}

pub(crate) async fn fetch_text(
    policy: &pagesift_policy::Policy,
    url: &Url,
    max_bytes: usize,
) -> Result<(Url, String)> {
    let mut request = pagesift_net::FetchRequest::new(url.as_str());
    request.max_body_bytes = Some(max_bytes);
    let response = policy
        .fetch(request, pagesift_policy::RobotsMode::Respect)
        .await?;
    anyhow::ensure!(
        response.status == 200 && !response.truncated,
        "incomplete input or HTTP status {}",
        response.status
    );
    let decoded = response.text();
    anyhow::ensure!(!decoded.had_errors, "input contains undecodable bytes");
    Ok((response.final_url, decoded.text))
}

pub(crate) fn read_bounded(file: Option<PathBuf>, max_bytes: usize) -> Result<String> {
    let reader: Box<dyn Read> = match file {
        Some(p) if p.as_os_str() != "-" => Box::new(std::fs::File::open(p)?),
        _ => Box::new(std::io::stdin()),
    };
    let mut bytes = Vec::new();
    reader.take(max_bytes as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= max_bytes, "input exceeds byte budget");
    Ok(String::from_utf8(bytes)?)
}

/// 每个 runtime worker 预留的虚拟地址空间（线程栈 + 运行时结构）。
/// 受限地址空间下按这个粒度折算可用线程数。
pub(crate) const ADDR_SPACE_PER_WORKER: u64 = 16 * 1024 * 1024;

/// 读 `/proc/self/limits` 的 "Max address space" 与当前 `VmSize`，
/// 在地址空间受限（如 `ulimit -v`）时给出还能开几个 worker。
/// 不受限或读不到时返回 `None`。
pub(crate) fn addr_space_worker_cap() -> Option<usize> {
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = limits
        .lines()
        .find(|l| l.starts_with("Max address space"))?;
    let soft = line.split_whitespace().nth(3)?;
    if soft == "unlimited" {
        return None;
    }
    let limit: u64 = soft.parse().ok()?;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let vm: u64 = status
        .lines()
        .find(|l| l.starts_with("VmSize:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?;
    let vm_bytes = vm * 1024;
    let free = limit.saturating_sub(vm_bytes);
    Some(((free / ADDR_SPACE_PER_WORKER) as usize).max(1))
}

/// 只处理一份输入的命令（`scrape`）用的运行时：一个调度线程，不探测 CPU 拓扑、不绑核。
/// [`runtime`] 的绑核是给独占机器的长任务（爬取、服务）用的；单份命令常是脚本里一份一个进程、
/// 十几个进程同时跑，每个进程都把自己的 worker 绑到同样的几个核上，再加上按核数起线程、
/// 读 sysfs 探测拓扑，这些开销都比转换一份文件本身大。CPU 密集的活（正文提取、PDF 解析）
/// 照样在阻塞线程池里做，许可数仍按核数。
pub(crate) fn light_runtime() -> Result<tokio::runtime::Runtime> {
    pagesift::set_cpu_capacity(std::thread::available_parallelism().map_or(4, |n| n.get()));
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime> {
    let topology = pagesift::adaptive::HardwareTopology::detect();
    let worker_cpus = topology.cpu_topology.runtime_worker_cpus();
    let mut workers = topology.runtime_workers.max(1);
    if let Some(cap) = addr_space_worker_cap() {
        workers = workers.min(cap);
    }
    // 必须在绑核之前：之后各线程只看得到自己那一个 CPU。
    pagesift::set_cpu_capacity(topology.runtime_workers);
    // 地址空间不够时线程会 spawn 失败（EAGAIN）。逐次减半重试，而不是让运行时 panic
    // 或卡在没有输出的状态里——受限环境下至少还能以小并发把活干完。
    loop {
        let next_cpu = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .worker_threads(workers)
            .thread_name("pagesift-worker");
        if !worker_cpus.is_empty() {
            let cpus = worker_cpus.clone();
            builder.on_thread_start(move || {
                use std::sync::atomic::Ordering;
                // 运行时先起 worker，之后按需起的是阻塞池线程（正文提取、PDF 解析）。
                // 只绑 worker：阻塞线程交给调度器放到空闲核上，免得和某个 worker 挤在同一个核。
                let index = next_cpu.fetch_add(1, Ordering::Relaxed);
                if index < workers {
                    let _ = pagesift::adaptive::pin_current_thread(cpus[index % cpus.len()]);
                }
            });
        }
        match builder.enable_all().build() {
            Ok(rt) => {
                if workers < topology.runtime_workers {
                    eprintln!(
                        "note: 地址空间受限，runtime worker 由 {} 降到 {workers}",
                        topology.runtime_workers
                    );
                }
                return Ok(rt);
            }
            Err(e) => {
                if workers <= 1 {
                    return Err(anyhow::anyhow!(
                        "starting the async runtime failed even with 1 worker: {e}"
                    ));
                }
                workers = (workers / 2).max(1);
            }
        }
    }
}

pub(crate) fn read_input(
    file: Option<PathBuf>,
    charset: Option<&str>,
) -> Result<(String, bool, &'static str)> {
    let bytes = match file {
        Some(path) if path.as_os_str() != "-" => {
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
        }
        _ => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("reading stdin")?;
            buf
        }
    };
    decode_input(&bytes, charset)
}

/// 解码本地 HTML：显式 --charset 优先，否则复用网络抓取路径的编码判定
/// （BOM → meta 预扫描 → 检测）。返回文本、是否有无法解码的字节、最终编码名。
pub(crate) fn decode_input(
    bytes: &[u8],
    charset: Option<&str>,
) -> Result<(String, bool, &'static str)> {
    if let Some(label) = charset {
        let label = label.trim();
        let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown charset {label:?}; use a WHATWG encoding label such as UTF-8, Shift_JIS or GBK"
            )
        })?;
        let (text, _, had_errors) = encoding.decode(bytes);
        return Ok((text.into_owned(), had_errors, encoding.name()));
    }
    let decoded = pagesift_net::decode(bytes, None, None);
    Ok((decoded.text, decoded.had_errors, decoded.encoding_name))
}

#[cfg(test)]
mod tests {
    use super::decode_input;

    #[test]
    fn decodes_local_html_from_meta_charset() {
        let text = "日本語の記事です。文字化けせずに読めます。";
        let html = format!(
            r#"<html><head><meta http-equiv="Content-Type" content="text/html; charset=Shift_JIS"></head><body><p>{text}</p></body></html>"#
        );
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&html);
        let (decoded, had_errors, encoding) = decode_input(bytes.as_ref(), None).unwrap();
        assert!(decoded.contains(text), "decoded as {encoding}: {decoded}");
        assert!(!had_errors);
        assert_eq!(encoding, "Shift_JIS");
    }

    #[test]
    fn explicit_charset_overrides_meta() {
        let text = "这是中文测试";
        let html = format!(
            r#"<html><head><meta charset="utf-8"></head><body><p>{text}</p></body></html>"#
        );
        let (bytes, _, _) = encoding_rs::GBK.encode(&html);
        let (decoded, had_errors, encoding) = decode_input(bytes.as_ref(), Some("gbk")).unwrap();
        assert!(decoded.contains(text));
        assert!(!had_errors);
        assert_eq!(encoding, "GBK");
    }

    #[test]
    fn rejects_unknown_charset() {
        let err = decode_input(b"<html></html>", Some("not-a-charset")).unwrap_err();
        assert!(err.to_string().contains("unknown charset"));
    }
}
