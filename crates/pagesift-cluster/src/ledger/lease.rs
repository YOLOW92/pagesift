//! 主网关的租约隔离：没有外部的锁服务，靠温备的确认续租。
//!
//! 温备每收到一批日志（含心跳）就回一个 `ShipAck`，带上它对这批的承诺：收到之后 `window` 内不接管
//! （`window` 是它当时的失联判定时限）。主网关记下每批的发出时刻 `s`（自己的时钟），收到确认后把租约
//! 延到 `s + window`；租约过期就拒绝一切写入（[`Ledger::writable`] 里检查）。温备只在
//! "最后收到一批的时刻 + 那时的 window + 一个心跳间隔"之后才接管（`Standby::deadline`）。
//! 一批在主网关发出之后才可能被温备收到，所以温备接管时主网关的租约已经过期，两边不会同时接受写入；
//! 只要求两台机器的时钟走速差别在一个心跳间隔所占的比例以内，不要求两边时钟对齐。
//!
//! 配了温备的主网关从"租约已过期"开始（[`Ledger::require_lease`]）：重启后要等温备确认过才接受写入，
//! 温备已经接管时旧主网关重启回来也不会再写。代价：温备停了，主网关也停止写入；
//! 宁要可用不要单写保证的部署关掉它（`GatewayConfig::fencing`）。

use std::sync::atomic::Ordering;

use super::Ledger;
use crate::types::{Deadline, Error};

/// 没有租约要求（单主机、温备、没配温备的网关）。
pub(super) const UNFENCED: u64 = u64::MAX;

impl Ledger {
    /// 从此要求租约；还没收到过确认时租约视为已过期。
    pub fn require_lease(&self) {
        let _ = self
            .lease_until
            .compare_exchange(UNFENCED, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    /// 温备确认后续租到 `until`（本账本时钟的毫秒）。没有租约要求时不做什么。
    pub fn extend_lease(&self, until: u64) {
        // 比较交换循环（`fetch_update` 在新版本里改名为 `try_update` 并弃用旧名，最低支持版本里还没有新名）
        let mut v = self.lease_until.load(Ordering::Acquire);
        while v != UNFENCED && v < until {
            match self.lease_until.compare_exchange_weak(
                v,
                until,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(now) => v = now,
            }
        }
    }

    /// 当前租约的到期时刻；没有租约要求时为 `None`。
    pub fn lease_until(&self) -> Option<Deadline> {
        match self.lease_until.load(Ordering::Acquire) {
            UNFENCED => None,
            v => Some(Deadline(v)),
        }
    }

    /// 本机现在持有租约（或没有租约要求）：可以写入，也可以做只该有一台机器做的事（监控调度）。
    pub fn holds_lease(&self) -> bool {
        self.check_lease().is_ok()
    }

    /// 租约过期（到期时刻及之后）时拒绝写入。
    pub(super) fn check_lease(&self) -> Result<(), Error> {
        let until = self.lease_until.load(Ordering::Acquire);
        if until != UNFENCED && self.clock.now_ms() >= until {
            return Err(Error::Rejected(
                "gateway lease expired: the standby has not acknowledged the journal recently, \
                 so it may be taking over; writes are refused until it acknowledges again"
                    .into(),
            ));
        }
        Ok(())
    }
}
