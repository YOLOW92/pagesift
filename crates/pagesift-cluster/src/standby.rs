//! 温备：持续接收主网关的日志并回放到自己的账本与存储；主网关失联后以
//! `term+1` 接管。
//!
//! 传送的是账本的逻辑日志（[`crate::journal`] 的记录），不是存储的物理段：回放走与在线路径
//! 相同的 `apply`，温备的存储里得到同样的日志，接管后它自己也能再带一个温备。
//! 只用到 store 已有的列表接口，不需要给 `pagesift-store` 加段级的增量读取。
//!
//! 失联判定：主网关空闲时按固定间隔发空批（心跳）。温备记录"以心跳结束的到达间隔"，
//! 判定时限取其 `q(1−δ)` 的置信上界（不低于心跳间隔本身）；样本还不够给出上界时取两个心跳间隔
//! （允许丢一个心跳；已有间隔样本时取其中最大者与心跳间隔的较大值）。最后一次收到消息后超过时限即判定失联。从未收到过主网关消息时不判定。
//!
//! 租约（协议版本 2 起，见 `ledger::lease`）：每收到一批就回 `ShipAck`，承诺收到后的这段时限内不接管；
//! 判定时刻因此不早于"承诺到期 + 一个心跳间隔"（心跳间隔是给两边时钟走速差的余量）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagesift_stat::sketch::Sketch;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

use crate::clock::ms;
use crate::frame::Msg;
use crate::ledger::{Ledger, upper};
use crate::transport::client::{Dial, open};
use crate::types::{Deadline, Error};

#[derive(Default)]
struct Beats {
    last: Option<u64>,
    gaps: Sketch,
    heartbeat: Duration,
    /// 对主网关承诺的不接管期限（本账本时钟的毫秒）：每次确认时的"收到时刻 + 时限"的最大值。
    promised: Option<u64>,
    /// 已确认的批数。
    batches: u64,
}

pub struct Standby {
    ledger: Arc<Ledger>,
    beats: Mutex<Beats>,
    applied: Notify,
}

impl Standby {
    /// `ledger` 用 [`Ledger::follower`] 打开，带自己的存储。
    pub fn new(ledger: Arc<Ledger>) -> Self {
        assert!(ledger.is_follower(), "standby needs a follower ledger");
        Self {
            ledger,
            beats: Mutex::new(Beats::default()),
            applied: Notify::new(),
        }
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    fn beats(&self) -> std::sync::MutexGuard<'_, Beats> {
        crate::types::lock(&self.beats)
    }

    /// 回放一批日志并记一次心跳。下标小于已回放长度的（重传）跳过。
    pub fn on_records(
        &self,
        start: u64,
        heartbeat: Duration,
        entries: &[Vec<u8>],
    ) -> Result<(), Error> {
        for (k, raw) in entries.iter().enumerate() {
            let i = start + k as u64;
            if i < self.ledger.shipped_len() {
                continue;
            }
            self.ledger.apply_shipped(i, raw)?;
        }
        let now = self.ledger.clock().now_ms();
        let mut b = self.beats();
        if let Some(last) = b.last
            && entries.is_empty()
        {
            b.gaps
                .push(Duration::from_millis(now.saturating_sub(last)).as_secs_f64());
        }
        b.last = Some(now);
        b.heartbeat = heartbeat;
        drop(b);
        self.applied.notify_waiters();
        Ok(())
    }

    /// 失联判定时限：最后一次收到消息后多久没消息算失联（见模块说明）。
    fn allowed(&self, b: &Beats) -> Duration {
        match upper(&b.gaps, self.ledger.delta()) {
            Some(q) => Duration::from_secs_f64(q).max(b.heartbeat),
            None => {
                b.heartbeat
                    .max(Duration::from_secs_f64(b.gaps.max().max(0.0)))
                    * 2
            }
        }
    }

    /// 判定主网关失联的时刻；从未收到消息时为 `None`。不早于对主网关承诺的租约期限加一个心跳间隔。
    pub fn deadline(&self) -> Option<Deadline> {
        let b = self.beats();
        let last = b.last?;
        let lost = last + ms(self.allowed(&b));
        let fenced = b.promised.map_or(0, |p| p + ms(b.heartbeat));
        Some(Deadline(lost.max(fenced)))
    }

    /// 记下对刚收到的一批的承诺，返回要回给主网关的确认。
    fn promise(&self) -> Msg {
        let now = self.ledger.clock().now_ms();
        let mut b = self.beats();
        let window = self.allowed(&b);
        b.batches += 1;
        b.promised = Some(b.promised.unwrap_or(0).max(now + ms(window)));
        Msg::ShipAck {
            batches: b.batches,
            window,
        }
    }

    pub fn primary_lost(&self) -> bool {
        self.deadline()
            .is_some_and(|d| self.ledger.clock().now_ms() > d.0)
    }

    /// 接管：账本任期加一，之后旧任期的日志与旧网关发出的租约令牌都被栅栏拒绝。
    /// 返回新任期；之后用同一个账本建 [`crate::GatewayCoordinator`] 对外服务。
    ///
    /// 回放上层状态（任务记录）失败过时拒绝（[`Ledger::app_failures`]）：这份副本与主网关不一致，
    /// 接管会让已完成的结果缺失、任务重复执行。要接管就从空的数据目录重新同步，或由运维确认后
    /// 手动把它作为网关启动。
    ///
    /// 这里的"失联"只是温备自己收不到心跳，没有外部的单写权威：主网关与温备之间断网、
    /// 但主网关仍能服务一部分工人时，两边会同时接受写入。只有确认旧主网关已停止或已隔离时，
    /// 自动接管才是安全的；不能确认时用手动接管（`[cluster] takeover = "manual"`）。
    pub fn take_over(&self) -> Result<u64, Error> {
        let failed = self.ledger.app_failures();
        if failed > 0 {
            return Err(Error::Rejected(format!(
                "{failed} replicated job records failed to apply on this standby; \
                 resync it from an empty data directory before it takes over"
            )));
        }
        self.ledger.take_over()
    }

    /// 等到回放的日志长度达到 `len`。
    pub async fn caught_up(&self, len: u64) {
        loop {
            let n = self.applied.notified();
            if self.ledger.shipped_len() >= len {
                return;
            }
            n.await;
        }
    }

    /// 连到主网关并持续回放，直到连接结束（主网关失联或关闭）。
    pub async fn follow<IO>(&self, io: IO, dial: &Dial) -> Result<(), Error>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut s = open(io, dial).await?;
        let from = self.ledger.shipped_len();
        s.out.send(1, &Msg::Ship { from })?;
        // 每个连接从头数批数（主网关那边也按连接数）
        self.beats().batches = 0;
        while let Some((_, m)) = s.reader.next().await? {
            match m {
                Msg::Records {
                    start,
                    heartbeat,
                    entries,
                } => {
                    self.on_records(start, heartbeat, &entries)?;
                    // [协议] 版本 2 起回确认（主网关的租约，见 `ledger::lease`）
                    if s.version >= 2 {
                        s.out.send(0, &self.promise())?;
                    }
                }
                Msg::Ack { r: Err(e) } => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::journal::{Record, encode};
    use crate::ledger::{AppState, LedgerConfig};

    struct Failing;

    impl AppState for Failing {
        fn apply(&self, _op: &[u8]) -> Result<(), String> {
            Err("disk full".into())
        }
        fn dump(&self) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }

    /// 回放任务记录失败过的温备不接管（0.90 只记一条警告，照样接管）。
    #[test]
    fn a_standby_with_unapplied_job_records_refuses_to_take_over() {
        let ledger =
            Arc::new(Ledger::follower(LedgerConfig::new(ManualClock::new(), false)).unwrap());
        assert!(ledger.set_app(Arc::new(Failing)));
        let sb = Standby::new(ledger.clone());
        let raw = encode(1, &Record::App { op: vec![1] });
        sb.on_records(0, Duration::from_secs(1), &[raw]).unwrap();
        assert_eq!(ledger.app_failures(), 1);
        let e = sb.take_over().unwrap_err().to_string();
        assert!(e.contains("failed to apply"), "{e}");
        assert!(ledger.is_follower());
    }
}
