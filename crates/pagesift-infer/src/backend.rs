//! 透明分流的接缝：模型直接调 [`crate::ops::matmul`]、[`crate::ops::conv2d`] 和 `nn` 里的层时，
//! 在 [`scoped`] 里运行就可能走加速后端，模型代码一个字不用改，公开签名也不变。
//!
//! 这一层每个算子都要把张量传过去再传回来，所以只对大算子划算；划不划算由后端按实测的传输开销和
//! 两边的算力决定，返回 `None` / `false` 表示“这个留在 CPU”。要连续在显存里跑或者拼批，用
//! [`crate::exec::Exec`]。

use std::cell::Cell;

use crate::{Activation, Tensor};

/// 加速后端。结果只允许和 CPU 版差浮点舍入。
pub trait Backend: Sync {
    /// `a[M, K] · b[K, N]`，行优先。`b_is_weight` 表示 `b` 是长期不变的权重，后端可以缓存。
    fn matmul(&self, a: &Tensor, b: &Tensor, b_is_weight: bool) -> Option<Vec<f32>>;

    /// 同 [`crate::ops::conv2d`]。
    #[allow(clippy::too_many_arguments)]
    fn conv2d(
        &self,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&[f32]>,
        stride: (usize, usize),
        pad: (usize, usize),
        dilation: (usize, usize),
        groups: usize,
    ) -> Option<Vec<f32>>;

    /// 原地激活一段内存，走了后端就返回 true。
    fn activate(&self, x: &mut [f32], a: Activation) -> bool;
}

thread_local! {
    /// 当前线程的后端。`'static` 是假的：只在 [`scoped`] 的作用域里存在，离开时恢复。
    static CURRENT: Cell<Option<&'static dyn Backend>> = const { Cell::new(None) };
}

/// 恢复上一层后端，panic 时也要执行。
struct Restore(Option<&'static dyn Backend>);

impl Drop for Restore {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

/// 在 `f` 运行期间，当前线程的算子优先走后端 `b`（`par` 的工作线程只算 CPU 内部的分块，不受影响）。
pub fn scoped<R>(b: &dyn Backend, f: impl FnOnce() -> R) -> R {
    // SAFETY: 引用只存进线程局部变量，Restore 保证 f 结束（含 panic）后被换掉，不会活过 b；
    // 取用都在 current() 里以调用期间的短借用进行
    let erased: &'static dyn Backend =
        unsafe { std::mem::transmute::<&dyn Backend, &'static dyn Backend>(b) };
    let _restore = Restore(CURRENT.with(|c| c.replace(Some(erased))));
    f()
}

/// 当前线程的后端。
#[inline]
pub(crate) fn current() -> Option<&'static dyn Backend> {
    CURRENT.with(|c| c.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops;

    /// 只认矩阵乘、把结果全写成 7 的假后端，用来确认分流路径和作用域。
    struct Seven;

    impl Backend for Seven {
        fn matmul(&self, a: &Tensor, b: &Tensor, _: bool) -> Option<Vec<f32>> {
            Some(vec![7.0; a.shape[0] * b.shape[1]])
        }

        fn conv2d(
            &self,
            _: &Tensor,
            _: &Tensor,
            _: Option<&[f32]>,
            _: (usize, usize),
            _: (usize, usize),
            _: (usize, usize),
            _: usize,
        ) -> Option<Vec<f32>> {
            None
        }

        fn activate(&self, _: &mut [f32], _: Activation) -> bool {
            false
        }
    }

    #[test]
    fn 只在作用域内分流且可以退回_cpu() {
        let a = Tensor::new(vec![1, 2], vec![1.0, 2.0]);
        let b = Tensor::new(vec![2, 1], vec![3.0, 4.0]);
        assert_eq!(ops::matmul(&a, &b).data, vec![11.0]);
        let got = scoped(&Seven, || {
            // 后端不接的算子照常在 CPU 上算
            let x = Tensor::new(vec![1, 1, 1], vec![2.0]);
            let w = Tensor::new(vec![1, 1, 1, 1], vec![3.0]);
            let y = ops::conv2d(&x, &w, None, (1, 1), (0, 0), (1, 1), 1);
            assert_eq!(y.data, vec![6.0]);
            ops::matmul(&a, &b).data
        });
        assert_eq!(got, vec![7.0]);
        assert_eq!(ops::matmul(&a, &b).data, vec![11.0]);
    }

    #[test]
    fn panic_后恢复() {
        let r = std::panic::catch_unwind(|| scoped(&Seven, || panic!("故意")));
        assert!(r.is_err());
        assert!(current().is_none());
    }
}
