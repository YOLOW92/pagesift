//! 浏览器内核档案：不同内核的启动参数、网络防护方式、标签页和回收策略，以及按优先级探测本机内核。
//!
//! 本机 wide / huge 压测（越高越好：页/秒；越低越好：PSS）：Lightpanda 约 65–70 页/秒、0.5 GB，
//! moli 约 29 页/秒、0.9 GB，chrome-headless-shell 约 33 页/秒、1.5 GB，Chrome 约 12 页/秒、2.3 GB。
//! Lightpanda 不渲染 iframe 内容，所以默认顺序是 Lightpanda → moli → Chrome 系，前一个不合适时回退。

use std::path::{Path, PathBuf};

use crate::launch;
use crate::pool::{BrowserSource, PoolConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// Chrome、Chromium、chrome-headless-shell、Edge：完整渲染，CDP 拦截子请求。
    Chrome,
    /// Lightpanda：最快最省内存，一个进程只能稳定渲染一页，不渲染 iframe。
    Lightpanda,
    /// moli：渲染和 Chrome 一致（含 iframe），比 Chrome 省内存；Fetch 拦截会卡死，由内核自己挡内网。
    Moli,
}

/// 内核的资源占用估计（MB）和每个进程大约占几个核，用来自动决定开几个进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footprint {
    pub browser_mb: u64,
    pub tab_mb: u64,
    pub cores_per_browser: usize,
}

impl EngineKind {
    /// 按可执行文件名判断内核，认不出的当作 Chrome 系。
    pub fn from_path(path: &Path) -> EngineKind {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name.starts_with("lightpanda") {
            EngineKind::Lightpanda
        } else if name.starts_with("moli") {
            EngineKind::Moli
        } else {
            EngineKind::Chrome
        }
    }

    pub fn parse(s: &str) -> Option<EngineKind> {
        match s.to_ascii_lowercase().as_str() {
            "chrome" | "chromium" => Some(EngineKind::Chrome),
            "lightpanda" => Some(EngineKind::Lightpanda),
            "moli" => Some(EngineKind::Moli),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            EngineKind::Chrome => "chrome",
            EngineKind::Lightpanda => "lightpanda",
            EngineKind::Moli => "moli",
        }
    }

    /// 渲染结果里是否带 iframe 的内容。
    pub fn renders_iframes(self) -> bool {
        self != EngineKind::Lightpanda
    }

    pub fn footprint(self) -> Footprint {
        match self {
            // 按本机压测的 PSS 估：16 个 Lightpanda 约 480 MB，16 个 moli 约 900 MB
            EngineKind::Lightpanda => Footprint {
                browser_mb: 40,
                tab_mb: 0,
                cores_per_browser: 1,
            },
            EngineKind::Moli => Footprint {
                browser_mb: 40,
                tab_mb: 40,
                cores_per_browser: 1,
            },
            EngineKind::Chrome => Footprint {
                browser_mb: 300,
                tab_mb: 200,
                cores_per_browser: 4,
            },
        }
    }

    /// 按内核调整池配置：启动参数、网络防护、标签页数和回收。已经给了 `args_override` 时只补
    /// 不影响参数的部分。`allow_private_network` 为 false 时内核挡掉全部内网地址；为 true 时
    /// 仍挡链路本地地址（云主机的元数据服务）。
    pub fn configure(self, cfg: &mut PoolConfig, allow_private_network: bool) {
        cfg.engine = Some(self);
        match self {
            EngineKind::Chrome => {
                if cfg.loopback_only {
                    cfg.extra_args.push(
                        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE *.localhost"
                            .into(),
                    );
                }
            }
            EngineKind::Lightpanda => {
                // 同一进程渲染第二页时会卡住，所以一个进程一页、用完就换
                cfg.max_tabs_per_browser = 1;
                cfg.recycle_after_pages = 1;
                if cfg.args_override.is_none() {
                    let mut args = serve_args();
                    if cfg.loopback_only {
                        args.extend(
                            ["0.0.0.0/0", "::/0", "-127.0.0.0/8", "-::1/128"]
                                .iter()
                                .flat_map(|c| ["--block-cidrs".to_string(), c.to_string()]),
                        );
                    } else if allow_private_network {
                        args.extend(
                            [
                                "--block-cidrs",
                                LINK_LOCAL_V4,
                                "--block-cidrs",
                                LINK_LOCAL_V6,
                            ]
                            .map(String::from),
                        );
                    } else {
                        args.push("--block-private-networks".into());
                    }
                    cfg.args_override = Some(args);
                    cfg.engine_network_guard = true;
                }
            }
            EngineKind::Moli => {
                cfg.max_tabs_per_browser = 1;
                if cfg.args_override.is_none() {
                    let mut args = serve_args();
                    if cfg.loopback_only {
                        // moli 不支持 `-` 豁免：列出 127.0.0.0/8 和 ::1 以外的全部地址
                        args.push(format!("--block-cidrs={MOLI_NON_LOOPBACK}"));
                    } else if allow_private_network {
                        // moli 只认一个逗号分隔的列表，不支持重复参数
                        args.push(format!("--block-cidrs={LINK_LOCAL_V4},{LINK_LOCAL_V6}"));
                    } else {
                        args.push("--block-private-networks".into());
                    }
                    cfg.args_override = Some(args);
                    cfg.engine_network_guard = true;
                }
            }
        }
    }
}

const MOLI_NON_LOOPBACK: &str = "0.0.0.0/2,64.0.0.0/3,96.0.0.0/4,112.0.0.0/5,120.0.0.0/6,\
124.0.0.0/7,126.0.0.0/8,128.0.0.0/1,2000::/3,fc00::/7,fe80::/10";
const LINK_LOCAL_V4: &str = "169.254.0.0/16";
const LINK_LOCAL_V6: &str = "fe80::/10";

fn serve_args() -> Vec<String> {
    ["serve", "--host", "127.0.0.1", "--port", launch::PORT]
        .map(String::from)
        .to_vec()
}

const LIGHTPANDA_NAMES: &[&str] = &["lightpanda"];
const MOLI_NAMES: &[&str] = &["moli"];
const HEADLESS_SHELL_NAMES: &[&str] = &["chrome-headless-shell"];

/// 本机能找到的内核，按默认优先级：Lightpanda、moli、chrome-headless-shell、Chrome/Chromium。
/// 每种内核只取第一个。
pub fn detect_engines() -> Vec<(EngineKind, PathBuf)> {
    let mut out = Vec::new();
    for (kind, names) in [
        (EngineKind::Lightpanda, LIGHTPANDA_NAMES),
        (EngineKind::Moli, MOLI_NAMES),
    ] {
        if let Some(p) = launch::find_in_path(names) {
            out.push((kind, p));
        }
    }
    if let Some(p) = launch::find_in_path(HEADLESS_SHELL_NAMES).or_else(launch::detect_browser) {
        out.push((EngineKind::Chrome, p));
    }
    out
}

/// 由一份基础配置展开成按优先级排列的池配置：
/// - 连接远程浏览器或自定义了启动参数时只有它自己；
/// - 指定了可执行文件时它排第一，后面跟本机探测到的其他种类的内核；
/// - 自动探测时是探测到的全部内核。都找不到时返回基础配置本身（启动时报找不到浏览器）。
pub fn engine_chain(base: &PoolConfig, allow_private_network: bool) -> Vec<PoolConfig> {
    let with = |kind: EngineKind, path: PathBuf| {
        let mut c = base.clone();
        c.source = BrowserSource::Executable(path);
        kind.configure(&mut c, allow_private_network);
        c
    };
    match &base.source {
        BrowserSource::Remote(_) => vec![base.clone()],
        BrowserSource::Executable(p) => {
            let kind = base.engine.unwrap_or_else(|| EngineKind::from_path(p));
            if base.args_override.is_some() {
                let mut c = base.clone();
                kind.configure(&mut c, allow_private_network);
                return vec![c];
            }
            let mut chain = vec![with(kind, p.clone())];
            chain.extend(
                detect_engines()
                    .into_iter()
                    .filter(|(k, q)| *k != kind && q != p)
                    .map(|(k, q)| with(k, q)),
            );
            chain
        }
        BrowserSource::Auto => {
            let found = detect_engines();
            if found.is_empty() || base.args_override.is_some() {
                return vec![base.clone()];
            }
            found.into_iter().map(|(k, p)| with(k, p)).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_executable_name() {
        for (path, kind) in [
            ("/opt/lightpanda-x86_64-linux", EngineKind::Lightpanda),
            ("/usr/local/bin/moli", EngineKind::Moli),
            ("/x/chrome-headless-shell", EngineKind::Chrome),
            ("/usr/bin/google-chrome", EngineKind::Chrome),
        ] {
            assert_eq!(EngineKind::from_path(Path::new(path)), kind, "{path}");
        }
    }

    #[test]
    fn engine_profiles_block_private_networks_natively() {
        let mut c = PoolConfig::default();
        EngineKind::Lightpanda.configure(&mut c, false);
        let args = c.args_override.clone().unwrap();
        assert!(args.contains(&"--block-private-networks".to_string()));
        assert!(args.contains(&"{port}".to_string()));
        assert!(c.engine_network_guard);
        assert_eq!((c.max_tabs_per_browser, c.recycle_after_pages), (1, 1));

        let mut c = PoolConfig::default();
        EngineKind::Moli.configure(&mut c, true);
        let args = c.args_override.unwrap();
        assert!(args.contains(&"--block-cidrs=169.254.0.0/16,fe80::/10".to_string()));
        assert!(!args.contains(&"--block-private-networks".to_string()));
        assert!(c.engine_network_guard);

        let mut c = PoolConfig::default();
        EngineKind::Chrome.configure(&mut c, false);
        assert!(c.args_override.is_none() && !c.engine_network_guard);
    }

    #[test]
    fn remote_and_custom_args_stay_single() {
        let remote = PoolConfig {
            source: BrowserSource::Remote("ws://127.0.0.1:9222".into()),
            ..PoolConfig::default()
        };
        assert_eq!(engine_chain(&remote, false).len(), 1);
        let custom = PoolConfig {
            source: BrowserSource::Executable("/x/lightpanda".into()),
            args_override: Some(vec!["serve".into(), "--block-private-networks".into()]),
            ..PoolConfig::default()
        };
        let chain = engine_chain(&custom, false);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].engine, Some(EngineKind::Lightpanda));
        assert_eq!(chain[0].max_tabs_per_browser, 1);
    }
}
