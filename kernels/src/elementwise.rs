//! 逐元素内核：加偏置与各激活函数，原地改写。语义与 `pagesift_infer::Activation::apply` 一致。

use crate::global_index;
use crate::math::{erf, expf};

/// `x[c, i] += bias[c]`：`x` 是 `[C, hw]`，共 `n = C·hw` 个元素。
#[unsafe(no_mangle)]
pub unsafe extern "ptx-kernel" fn add_bias(x: *mut f32, bias: *const f32, hw: u32, n: u32) {
    let i = global_index();
    if i < n as u64 {
        // u32 除法：64 位除法在 GPU 上要展开成上百条指令
        let i = i as u32;
        *x.add(i as usize) += *bias.add((i / hw) as usize);
    }
}

macro_rules! unary {
    ($(#[$doc:meta])* $name:ident, |$v:ident| $body:expr) => {
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub unsafe extern "ptx-kernel" fn $name(x: *mut f32, n: u32) {
            let i = global_index();
            if i < n as u64 {
                let p = x.add(i as usize);
                let $v: f32 = *p;
                *p = $body;
            }
        }
    };
}

unary!(
    /// max(x, 0)；NaN 给 0，同 `f32::max`
    relu, |v| v.max(0.0)
);
unary!(
    /// x · σ(x) = x / (1 + e^−x)
    silu, |v| v / (1.0 + expf(-v))
);
unary!(
    /// 精确 GELU：x/2 · (1 + erf(x/√2))
    gelu, |v| 0.5 * v * (1.0 + erf(v * core::f32::consts::FRAC_1_SQRT_2))
);
unary!(
    /// relu6(x + 3) / 6
    // [协议] PyTorch Hardsigmoid 的定义：0（x ≤ −3）、1（x ≥ 3）、x/6 + 1/2
    hardsigmoid, |v| ((v + 3.0) / 6.0).clamp(0.0, 1.0)
);
unary!(
    /// 1 / (1 + e^−x)
    sigmoid, |v| 1.0 / (1.0 + expf(-v))
);
