//! `[cluster]` 配置段：多服务器部署的角色、地址、令牌和 TLS。没有这一段时是单主机。
//!
//! ```toml
//! [cluster]
//! role = "gateway"            # standalone（默认）/ gateway / worker / standby
//! listen = "0.0.0.0:7700"     # 网关、温备：接工人连接的地址
//! journal = true              # 网关：写账本日志（温备要用）
//! [cluster.tls]
//! cert = "gw.pem"             # 网关证书链（温备接管后也用它）
//! key = "gw.key"
//! client_ca = "workers-ca.pem" # 可选：要求工人出示由它签发的证书（双向 TLS）
//! [[cluster.workers]]
//! id = "w1"
//! token_hash = "…"            # 工人启动时在日志里打印自己令牌的哈希
//! ```
//!
//! 工人一侧：`role = "worker"`、`id`、`token`（或 `{ env = "VAR" }`）、`gateways = ["gw1:7700", "gw2:7700"]`，
//! `[cluster.tls] ca`（不写用系统根证书）、`server_name`，双向 TLS 时 `cert` + `key` 是客户端证书。
//! 温备：`role = "standby"`、`id`、`token`、`primary`，以及接管后作为网关要用的 `listen`、`[cluster.tls]`、
//! `[[cluster.workers]]`。不用 TLS 时必须显式写 `h2c = true`（只用于可信内网）。
//! 主网关默认用租约隔离（`fencing = true`）：温备每收到一批日志就确认，主网关的租约靠确认续期，
//! 过期就停止写入；温备只在租约必然过期之后接管，所以主备之间断网时两边也不会同时写
//! （见 `pagesift_cluster::ledger::lease`）。代价是温备停了主网关也停写；`fencing = false` 换回可用性，
//! 断网时可能两边同时写。`takeover = "manual"` 时温备判定失联后不自动接管，只报告，由运维处理。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::SecretValue;
use crate::cluster::{ClientTls, Link};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cluster {
    /// `standalone`（默认，也写作 `all`）、`gateway`、`worker`、`standby`；命令行 `--role` 优先。
    pub role: Option<String>,
    /// 工人、温备在集群里的 id（网关按它登记令牌、算站点归属）。
    pub id: Option<String>,
    /// 工人、温备连网关用的令牌。
    pub token: Option<SecretValue>,
    /// 网关、温备：接工人连接的地址。
    pub listen: Option<String>,
    /// 工人：网关地址，按顺序尝试（主网关在前、温备在后）。
    #[serde(default)]
    pub gateways: Vec<String>,
    /// 温备：主网关地址。
    pub primary: Option<String>,
    /// 网关：温备的 id（它的令牌也要在 `[[cluster.workers]]` 里登记）。设了就自动打开账本日志，
    /// 任务记录随日志复制到温备。
    pub standby: Option<String>,
    /// 温备：判定主网关失联后 `auto`（默认）自动接管，`manual` 只报告、等运维处理（见模块说明）。
    pub takeover: Option<String>,
    /// 网关：配了 `standby` 时用租约隔离（默认开）：温备的确认停了就停止写入，温备接管时两边不会同时写；
    /// 代价是温备停了主网关也停写。`false` 时温备停了照常写，但主备之间断网时可能两边同时写。
    pub fencing: Option<bool>,
    /// 网关：把账本写进数据目录的日志（网关重启后从它恢复租约；日志按快照压缩）。
    /// 默认：设了 `standby` 时开，否则关。
    pub journal: Option<bool>,
    /// 网关、温备（接管后）：MCP 的 http 端口（只监听本机），抓取和任务走网关的任务系统。不写就不开。
    pub mcp_port: Option<u16>,
    /// 明文 HTTP/2（h2c）：只用于部署者确认可信的内网，必须显式打开。
    #[serde(default)]
    pub h2c: bool,
    #[serde(default)]
    pub tls: ClusterTls,
    /// 网关（和接管后的温备）：允许连接的工人与温备，只存令牌哈希。
    #[serde(default)]
    pub workers: Vec<ClusterWorker>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterTls {
    /// 网关：证书链；工人：双向 TLS 的客户端证书。PEM。
    pub cert: Option<PathBuf>,
    /// `cert` 的私钥（PEM）。
    pub key: Option<PathBuf>,
    /// 工人、温备：校验网关证书的 CA（PEM）；不写时用系统根证书。
    pub ca: Option<PathBuf>,
    /// 网关：校验工人证书的 CA（PEM）；写了就要求双向 TLS。
    pub client_ca: Option<PathBuf>,
    /// 工人、温备：网关证书上的名字；不写时取地址的主机部分。
    pub server_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterWorker {
    pub id: String,
    /// 令牌的哈希（十六进制），工人启动时会在日志里打印。
    pub token_hash: String,
}

/// 服务的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Standalone,
    Gateway,
    Worker,
    Standby,
}

impl Role {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "standalone" | "all" => Self::Standalone,
            "gateway" => Self::Gateway,
            "worker" => Self::Worker,
            "standby" => Self::Standby,
            other => bail!(
                "unknown cluster role {other:?} (expected standalone, gateway, worker or standby)"
            ),
        })
    }
}

impl Cluster {
    /// 生效的角色：命令行优先，其次配置，默认单主机。
    pub fn role(&self, cli: Option<&str>) -> Result<Role> {
        cli.or(self.role.as_deref())
            .map_or(Ok(Role::Standalone), Role::parse)
    }

    fn need<'a, T>(&self, v: &'a Option<T>, what: &str) -> Result<&'a T> {
        v.as_ref()
            .with_context(|| format!("[cluster] {what} is required for this role"))
    }

    pub fn listen(&self) -> Result<&str> {
        self.need(&self.listen, "listen").map(String::as_str)
    }

    pub fn primary(&self) -> Result<&str> {
        self.need(&self.primary, "primary").map(String::as_str)
    }

    /// 温备是否只在运维确认后接管（`takeover = "manual"`）。
    pub fn manual_takeover(&self) -> Result<bool> {
        match self.takeover.as_deref() {
            None | Some("auto") => Ok(false),
            Some("manual") => Ok(true),
            Some(other) => {
                bail!("[cluster] takeover = {other:?} (expected \"auto\" or \"manual\")")
            }
        }
    }

    /// 登记的工人令牌（只有哈希）。
    pub fn tokens(&self) -> Result<pagesift_cluster::TokenStore> {
        let t = pagesift_cluster::TokenStore::new();
        for w in &self.workers {
            let h = blake3::Hash::from_hex(w.token_hash.trim()).with_context(|| {
                format!(
                    "[[cluster.workers]] {}: token_hash is not a BLAKE3 hex digest",
                    w.id
                )
            })?;
            t.add_hash(pagesift_cluster::WorkerId::new(w.id.as_str()), h);
        }
        Ok(t)
    }

    fn check_transport(&self) -> Result<()> {
        let t = &self.tls;
        if self.h2c && (t.cert.is_some() || t.client_ca.is_some() || t.ca.is_some()) {
            bail!("[cluster] h2c = true conflicts with the settings in [cluster.tls]");
        }
        Ok(())
    }

    /// 网关一侧的 TLS 配置；`h2c = true` 时为 `None`。
    pub fn server_tls(&self) -> Result<Option<Arc<tokio_rustls::rustls::ServerConfig>>> {
        self.check_transport()?;
        if self.h2c {
            return Ok(None);
        }
        let (Some(cert), Some(key)) = (&self.tls.cert, &self.tls.key) else {
            bail!(
                "[cluster.tls] cert and key are required for the gateway (or set h2c = true on a trusted network)"
            );
        };
        crate::cluster::net::server_tls(cert, key, self.tls.client_ca.as_deref()).map(Some)
    }

    /// 工人、温备连网关用的身份与传输。
    pub fn link(&self) -> Result<Link> {
        self.check_transport()?;
        let id = self.need(&self.id, "id")?;
        let token = self.need(&self.token, "token")?.resolve()?;
        let tls = if self.h2c {
            None
        } else {
            let identity = match (&self.tls.cert, &self.tls.key) {
                (Some(c), Some(k)) => Some((c.as_path(), k.as_path())),
                (None, None) => None,
                _ => bail!("[cluster.tls] cert and key must be given together"),
            };
            Some(ClientTls {
                config: crate::cluster::net::client_tls(self.tls.ca.as_deref(), identity)?,
                server_name: self.tls.server_name.clone(),
            })
        };
        Ok(Link {
            id: pagesift_cluster::WorkerId::new(id.as_str()),
            token,
            tls,
        })
    }
}
