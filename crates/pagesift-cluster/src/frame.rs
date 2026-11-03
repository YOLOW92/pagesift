//! 工人 ↔ 网关、温备 ↔ 主网关之间的消息与帧格式。
//!
//! 帧：`[长度 u32 小端][类型 u8][关联号 u64 小端][负载]`，长度不含自身的 4 字节。
//! 请求与应答用同一个关联号配对；关联号 0 是对方主动推送（成员表）或不需要应答的消息（统计、交接）。
//! 负载编码见 [`crate::codec`]：与 `Stat::encode` 的统计增量同一套 `Writer`/`Reader`，
//! 增量直接嵌进帧里不必再转一次。
//!
//! 协议版本在 HTTP 头里协商（见 [`crate::transport`]），帧格式本身不带版本。
//! 工人连接的第一帧是 `Hello`，温备连接的第一帧是 `Ship`。

use std::time::Duration;

use pagesift_stat::{DecodeError, Reader, Writer};

use crate::codec::{codec_enum, codec_struct};
use crate::hrw::HostMap;
use crate::types::{
    Error, Fence, Renewal, Resources, Scope, SiteState, StatKind, StatsDelta, Task, TaskId,
    WorkerId,
};

/// [协议] 帧头：4 字节长度
pub const LEN_BYTES: usize = 4;

/// 线上的租约（工人收到后按本地时钟换算成 [`crate::types::Lease`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct WireLease {
    pub task: TaskId,
    pub fence: Fence,
    pub ttl: Duration,
    pub work: Task,
    pub handoff: Option<SiteState>,
}

codec_struct! {
    WireLease { task, fence, ttl, work, handoff }
}

codec_enum! {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Msg {
        /// 握手：工人 id、本次启动编号、相对容量、握手往返时间（冷启动的往返上界）、硬件档案。
        1 => Hello { worker: WorkerId, boot: u64, weight: f64, rtt: Duration, profile: Vec<u8> }, // [协议] 消息标签
        /// 握手应答：网关的任期与当前成员表。
        2 => Welcome { term: u64, map: HostMap }, // [协议] 同上
        /// `capacity` 是工人当前的准入容量（每次拉取都报，网关据此汇总集群容量）。
        3 => Pull { free: Resources, rtt_hi: Duration, capacity: f64 }, // [协议] 同上
        4 => Granted { lease: Option<WireLease> }, // [协议] 同上
        5 => Renew { task: TaskId, fence: Fence, rtt_hi: Duration }, // [协议] 同上
        6 => Renewed { r: Result<Renewal, Error> }, // [协议] 同上
        7 => Commit { task: TaskId, fence: Fence, ran: Duration, result: Vec<u8> }, // [协议] 同上
        8 => Submit { task: Task }, // [协议] 同上
        9 => Submitted { r: Result<TaskId, Error> }, // [协议] 同上
        10 => Cancel { task: TaskId }, // [协议] 同上
        11 => Cancelled { r: Result<bool, Error> }, // [协议] 同上
        /// 通用应答（提交、握手被拒）。
        12 => Ack { r: Result<(), Error> }, // [协议] 同上
        13 => Stats { delta: StatsDelta }, // [协议] 同上
        14 => PriorsReq { scope: Scope }, // [协议] 同上
        15 => Priors { tables: Vec<(StatKind, Vec<u8>)> }, // [协议] 同上
        16 => Handoff { state: SiteState }, // [协议] 同上
        /// 网关推送的成员表。
        17 => HostMap { map: HostMap }, // [协议] 同上
        /// 温备请求从下标 `from` 起的日志（温备连接的第一帧）。
        18 => Ship { from: u64 }, // [协议] 同上
        /// 一批日志（空批是心跳），`heartbeat` 是主网关的心跳间隔。
        19 => Records { start: u64, heartbeat: Duration, entries: Vec<Vec<u8>> }, // [协议] 同上
        /// 温备确认收到了第 `batches` 批（从 1 数，含心跳），并承诺在收到它之后 `window` 内不接管
        /// （协议版本 2 起；主网关据此续租，见 `ledger::lease`）。
        20 => ShipAck { batches: u64, window: Duration }, // [协议] 同上
    }
}

/// 编码一帧（含长度前缀）。
pub fn encode(corr: u64, m: &Msg) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(0);
    w.u8(m.tag());
    w.u64(corr);
    m.put_body(&mut w);
    let mut buf = w.into_inner();
    let len = u32::try_from(buf.len() - LEN_BYTES).expect("frame longer than u32::MAX");
    buf[..LEN_BYTES].copy_from_slice(&len.to_le_bytes());
    buf
}

/// 解码一帧的内容（不含长度前缀），返回（关联号，消息）。
pub fn decode(body: &[u8]) -> Result<(u64, Msg), DecodeError> {
    let mut r = Reader::new(body);
    let tag = r.u8()?;
    let corr = r.u64()?;
    let m = Msg::get_body(tag, &mut r)?;
    if !r.is_empty() {
        return Err(DecodeError::Invalid("frame trailing bytes"));
    }
    Ok((corr, m))
}

/// 读出 `buf` 开头的一个完整帧：返回帧内容的长度区间；不完整时为 `None`；超过 `max` 时报错。
pub fn split(buf: &[u8], max: usize) -> Result<Option<std::ops::Range<usize>>, Error> {
    let Some(head) = buf.first_chunk::<LEN_BYTES>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*head) as usize;
    if len > max {
        return Err(Error::Transport(format!(
            "frame of {len} bytes exceeds {max}"
        )));
    }
    Ok((buf.len() >= LEN_BYTES + len).then_some(LEN_BYTES..LEN_BYTES + len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Codec;
    use crate::types::{Deadline, Resource, SiteKey};

    #[test]
    fn every_message_round_trips() {
        let task = Task {
            site: SiteKey(3),
            class: 1,
            timeout: Duration::from_secs(5),
            need: Resources::default().with(Resource::Memory, 1e6),
            body: b"https://example.org/".to_vec(),
        };
        let fence = Fence { term: 3, epoch: 2 };
        let state = SiteState {
            site: SiteKey(3),
            wait: Duration::from_millis(250),
            state: vec![9; 4],
        };
        let msgs = vec![
            Msg::Hello {
                worker: WorkerId::new("w"),
                boot: 5,
                weight: 2.0,
                rtt: Duration::from_millis(3),
                profile: vec![1],
            },
            Msg::Welcome {
                term: 4,
                map: HostMap::new(9, [(WorkerId::new("a"), 1.0)]),
            },
            Msg::Pull {
                free: Resources::default().with(Resource::Cpu, 2.0),
                rtt_hi: Duration::from_millis(7),
                capacity: 3.0,
            },
            Msg::Granted { lease: None },
            Msg::Granted {
                lease: Some(WireLease {
                    task: TaskId(1 << 20),
                    fence,
                    ttl: Duration::from_secs(9),
                    work: task.clone(),
                    handoff: Some(state.clone()),
                }),
            },
            Msg::Renew {
                task: TaskId(4),
                fence,
                rtt_hi: Duration::from_millis(1),
            },
            Msg::Renewed {
                r: Ok(Renewal::Cancel),
            },
            Msg::Renewed {
                r: Ok(Renewal::Extended {
                    expires: Deadline(77),
                    ttl: Duration::from_secs(1),
                }),
            },
            Msg::Renewed {
                r: Err(Error::Fenced { current: fence }),
            },
            Msg::Commit {
                task: TaskId(4),
                fence,
                ran: Duration::from_millis(40),
                result: vec![0; 100],
            },
            Msg::Submit { task },
            Msg::Submitted { r: Ok(TaskId(8)) },
            Msg::Submitted {
                r: Err(Error::Rejected("x".into())),
            },
            Msg::Cancel { task: TaskId(8) },
            Msg::Cancelled { r: Ok(true) },
            Msg::Ack { r: Ok(()) },
            Msg::Ack {
                r: Err(Error::Closed),
            },
            Msg::Ack {
                r: Err(Error::Store("disk".into())),
            },
            Msg::Ack {
                r: Err(Error::Transport("t".into())),
            },
            Msg::Ack {
                r: Err(Error::Cancelled),
            },
            Msg::Ack {
                r: Err(Error::UnknownTask),
            },
            Msg::Stats {
                delta: StatsDelta {
                    kind: StatKind(2),
                    seq: 1,
                    table: vec![5],
                },
            },
            Msg::PriorsReq {
                scope: Scope::Family(6),
            },
            Msg::Priors {
                tables: vec![(StatKind(2), vec![1, 2])],
            },
            Msg::Handoff { state },
            Msg::HostMap {
                map: HostMap::default(),
            },
            Msg::Ship { from: 12 },
            Msg::Records {
                start: 12,
                heartbeat: Duration::from_secs(1),
                entries: vec![vec![1], vec![]],
            },
            Msg::ShipAck {
                batches: 3,
                window: Duration::from_millis(2500),
            },
        ];
        for (i, m) in msgs.into_iter().enumerate() {
            let buf = encode(i as u64 + 1, &m);
            let range = split(&buf, usize::MAX).unwrap().unwrap();
            assert_eq!(range.end, buf.len());
            assert_eq!(decode(&buf[range]).unwrap(), (i as u64 + 1, m));
            assert_eq!(split(&buf[..buf.len() - 1], usize::MAX).unwrap(), None);
        }
        let big = encode(1, &Msg::Ship { from: 0 });
        assert!(split(&big, 1).is_err(), "oversized frame is rejected");
        // 字节串与 `Writer::bytes` 的格式相同（长度 + 原样字节）
        let mut a = Writer::new();
        vec![7u8, 8].put(&mut a);
        let mut b = Writer::new();
        b.bytes(&[7, 8]);
        assert_eq!(a.into_inner(), b.into_inner());
    }
}
