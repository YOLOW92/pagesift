//! 一次转换的输出预算：Word、Excel、PowerPoint、CSV、EPUB 共用一个计数器。
//!
//! 压缩包、CSV 里的一个字段都能用很小的输入展开出几十 MB 的文字（434 KB 的 DOCX 展开成 81 MB
//! Markdown）。HTML 有响应体上限、PDF 有页数上限，这几种格式也要有：文字累计超过
//! [`crate::Limits::max_output_bytes`] 就停止转换，已经转出的部分照常返回，另加 `output_truncated` 警告。
//!
//! 计数放在共享的地方，不给每种格式写专门逻辑：XML 读取器给出的每段文字（DOCX、XLSX、PPTX）、
//! CSV 的每一行、EPUB 的每一章。预算用完后 XML 读取器报告文档结束，各格式的循环自然收尾。
//! 一份文档只在一个线程里转换，所以放在线程局部变量里（和 PDF 的截止时间一样）。

use std::cell::Cell;

thread_local! {
    static LEFT: Cell<usize> = const { Cell::new(usize::MAX) };
    static HIT: Cell<bool> = const { Cell::new(false) };
}

/// 开始一次转换，返回的守卫离开作用域时恢复原值。
pub(crate) fn enter(limit: usize) -> Guard {
    Guard {
        left: LEFT.with(|l| l.replace(limit)),
        hit: HIT.with(|h| h.replace(false)),
    }
}

pub(crate) struct Guard {
    left: usize,
    hit: bool,
}

impl Guard {
    /// 这次转换是不是因为预算用完而提前停下了。
    pub(crate) fn truncated(&self) -> bool {
        HIT.with(Cell::get)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        LEFT.with(|l| l.set(self.left));
        HIT.with(|h| h.set(self.hit));
    }
}

/// 记下产出了 `n` 字节的文字；还有余量时返回 true。
pub(crate) fn spend(n: usize) -> bool {
    LEFT.with(|l| {
        let left = l.get();
        if n >= left {
            l.set(0);
            HIT.with(|h| h.set(true));
            false
        } else {
            l.set(left - n);
            true
        }
    })
}

/// 预算已经用完。
pub(crate) fn exhausted() -> bool {
    HIT.with(Cell::get)
}

/// 批量记账：调用方先在本地累加，攒够这么多再记一次，免得每段文字都访问线程局部变量。
pub(crate) const BATCH: usize = 64 << 10;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spending_past_the_limit_marks_the_conversion_truncated() {
        let g = enter(100);
        assert!(spend(60));
        assert!(!exhausted());
        assert!(!spend(40));
        assert!(exhausted() && g.truncated());
        drop(g);
        // 守卫恢复了外层（没有预算）的状态
        assert!(!exhausted());
        assert!(spend(usize::MAX / 2));
    }
}
