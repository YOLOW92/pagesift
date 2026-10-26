//! 本次提取的截止时间（`Options::timeout`），给解释内容流之前的阶段用：文件修复扫描、结构树。
//! 一份文件只在一个线程里处理，所以放在线程局部变量里，免得一层层传参数。

use std::cell::Cell;
use std::time::Instant;

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 设置截止时间，返回的守卫离开作用域时恢复原值。
pub(crate) fn enter(deadline: Option<Instant>) -> Guard {
    Guard(DEADLINE.with(|d| d.replace(deadline)))
}

pub(crate) struct Guard(Option<Instant>);

impl Drop for Guard {
    fn drop(&mut self) {
        DEADLINE.with(|d| d.set(self.0));
    }
}

/// 已经过了截止时间。
pub(crate) fn expired() -> bool {
    DEADLINE.with(|d| d.get().is_some_and(|t| Instant::now() >= t))
}
