//! 上层状态随账本复制：网关把 API 任务记录的每次写入作为不透明的 `App` 记录追加进日志，
//! 日志传送原样送到温备，温备交给 [`AppState::apply`] 写进自己的存储；快照时
//! [`AppState::dump`] 给出上层的全部状态（`SnapApp`），日志截断后温备仍能从快照完整重建。
//!
//! 上层的写入要满足：同一个键按日志顺序重放的结果与原写入一致，且重复应用无害
//! （快照与在线写入之间可能重叠）。

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::Ledger;
use crate::journal::Record;
use crate::types::Error;

/// 随账本复制的上层状态。
pub trait AppState: Send + Sync {
    /// 应用一条写入（温备回放主网关的日志、或载入快照时）。失败时返回原因：账本记下
    /// 上层状态不完整（[`Ledger::app_failures`]），温备因此不自动接管。
    fn apply(&self, op: &[u8]) -> Result<(), String>;
    /// 全部状态，编码成若干条写入（快照时调用，此时账本的锁都已拿住：实现不能再调用账本）。
    fn dump(&self) -> Vec<Vec<u8>>;
}

impl Ledger {
    /// 装上上层状态（只能装一次）。
    pub fn set_app(&self, app: Arc<dyn AppState>) -> bool {
        self.app.set(app).is_ok()
    }

    /// 追加一条上层写入（网关）。不在这里做快照：调用方可能正拿着自己的锁，
    /// 而快照要回调 [`AppState::dump`]；字节计数照常累计，下一次账本操作时再判断是否快照。
    pub fn append_app(&self, op: Vec<u8>) -> Result<(), Error> {
        let term = self.writable()?;
        self.log(term, &Record::App { op })
    }

    pub(super) fn apply_app(&self, op: &[u8]) {
        if self.is_follower()
            && let Some(app) = self.app.get()
            && app.apply(op).is_err()
        {
            self.app_failures.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// 回放上层写入失败的次数（自打开账本）。不为 0 时温备上的任务记录与主网关不一致。
    pub fn app_failures(&self) -> u64 {
        self.app_failures.load(Ordering::SeqCst)
    }
}
