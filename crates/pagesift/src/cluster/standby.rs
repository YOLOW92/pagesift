//! 温备：连主网关接收账本日志并回放（`Standby::follow`）；判定主网关失联后 `take_over()`，
//! 之后由调用方用同一个账本作为网关对外服务。
//!
//! 连接断开不等于失联：先按 `Standby::deadline`（心跳间隔分布的 `q(1−δ)` 上界）等到判定时刻，
//! 其间重连成功就继续回放；到时刻仍连不上才接管。从未收到过主网关消息时不接管
//! （与 `pagesift-cluster` 的判定一致），按主网关的心跳间隔重试连接。
//!
//! 失联只是温备自己的判断，没有外部的单写权威：主备之间断网、旧主网关仍在服务一部分工人时，
//! 自动接管会让两边同时接受写入（脑裂）。`manual` 时判定失联后只报告并继续重连，
//! 由运维隔离旧主网关后，用同一个数据目录以 `--role gateway`（`[cluster] journal = true`）重启温备。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use pagesift_cluster::{GatewayConfig, Ledger, Standby};

use super::net::{Link, dial};

pub struct StandbyOptions {
    /// 主网关地址（`host:port`）。
    pub primary: String,
    pub link: Link,
    /// 判定失联后不自动接管（`[cluster] takeover = "manual"`）。
    pub manual: bool,
}

/// 跟随主网关直到它失联，然后接管并返回新任期；`shutdown` 先完成时返回 `None`。
/// `ledger` 用 `Ledger::follower` 打开，带温备自己的存储。
pub async fn follow_until_lost(
    ledger: Arc<Ledger>,
    opts: &StandbyOptions,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<Option<u64>> {
    let sb = Standby::new(ledger);
    let d = opts.link.dial(&opts.primary);
    tokio::pin!(shutdown);
    let mut reported = false;
    loop {
        let session = async {
            let io = dial(&opts.primary, &opts.link).await?;
            sb.follow(io, &d).await.map_err(anyhow::Error::from)
        };
        let r = tokio::select! {
            () = &mut shutdown => return Ok(None),
            r = session => r,
        };
        if let Err(e) = r {
            tracing::warn!("standby: following {}: {e:#}", opts.primary);
        }
        let lost = sb.primary_lost();
        if lost && !opts.manual {
            break;
        }
        if lost && !reported {
            reported = true;
            tracing::error!(
                "standby: primary {} lost; not taking over (takeover = \"manual\"). Fence the old \
                 primary, then restart this node with --role gateway and the same data directory",
                opts.primary
            );
        }
        if !lost {
            reported = false;
        }
        let wait = match sb.deadline() {
            // 等到判定时刻之后（多 1 ms 保证越过）再试一次；已判定失联（手动接管）时按心跳间隔重连
            Some(t) if !lost => {
                Duration::from_millis(t.0.saturating_sub(sb.ledger().clock().now_ms()) + 1)
            }
            _ => GatewayConfig::default().heartbeat,
        };
        tokio::select! {
            () = &mut shutdown => return Ok(None),
            () = tokio::time::sleep(wait) => {}
        }
    }
    let term = sb.take_over()?;
    tracing::warn!(
        "standby: primary {} lost, took over with term {term}",
        opts.primary
    );
    Ok(Some(term))
}
