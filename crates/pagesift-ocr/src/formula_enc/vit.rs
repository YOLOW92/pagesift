//! ViT 层（参考实现的 `PPFormulaNetVisionLayer`，即 SAM 图像编码器的 Block）：
//!
//! ```text
//! x = x + unpartition(attn(partition(ln1(x))))   # 窗口层；全局层不切窗口
//! x = x + lin2(gelu(lin1(ln2(x))))
//! ```
//!
//! 切窗口前在右、下补 0（补在 LayerNorm 之后），补出来的 token 也参与注意力（作为键和值），
//! 还原时裁掉。

use pagesift_infer::{Activation, LayerNorm, Linear, Tensor, Weights, ops};

use super::attention::Attention;

/// 把 `[h·w, c]`（行优先的 h×w 网格）切成 `win×win` 的窗口，右、下补 0 到 `win` 的倍数。
/// 返回 `[nh·nw·win², c]`（窗口按行优先排，窗口内也按行优先）和补边后的 (高, 宽)。
pub(crate) fn partition(
    x: &[f32],
    h: usize,
    w: usize,
    c: usize,
    win: usize,
) -> (Vec<f32>, (usize, usize)) {
    let (ph, pw) = (h.next_multiple_of(win), w.next_multiple_of(win));
    let (nh, nw) = (ph / win, pw / win);
    let mut out = vec![0f32; ph * pw * c];
    for wy in 0..nh {
        for wx in 0..nw {
            let base = (wy * nw + wx) * win * win;
            for iy in 0..win {
                let y = wy * win + iy;
                if y >= h {
                    break;
                }
                for ix in 0..win {
                    let xx = wx * win + ix;
                    if xx >= w {
                        break;
                    }
                    let src = (y * w + xx) * c;
                    let dst = (base + iy * win + ix) * c;
                    out[dst..dst + c].copy_from_slice(&x[src..src + c]);
                }
            }
        }
    }
    (out, (ph, pw))
}

/// [`partition`] 的逆：窗口拼回 `ph × pw` 的网格，再裁成 `h × w`，返回 `[h·w, c]`。
pub(crate) fn unpartition(
    win_data: &[f32],
    (ph, pw): (usize, usize),
    h: usize,
    w: usize,
    c: usize,
    win: usize,
) -> Vec<f32> {
    let nw = pw / win;
    debug_assert_eq!(win_data.len(), ph * pw * c);
    let mut out = vec![0f32; h * w * c];
    for y in 0..h {
        let (wy, iy) = (y / win, y % win);
        for x in 0..w {
            let (wx, ix) = (x / win, x % win);
            let src = ((wy * nw + wx) * win * win + iy * win + ix) * c;
            let dst = (y * w + x) * c;
            out[dst..dst + c].copy_from_slice(&win_data[src..src + c]);
        }
    }
    out
}

/// 一层 ViT。
pub(crate) struct Layer {
    ln1: LayerNorm,
    attn: Attention,
    ln2: LayerNorm,
    lin1: Linear,
    lin2: Linear,
    /// 窗口边长；0 表示全局注意力
    window: usize,
}

impl Layer {
    /// 读 `{p}.layer_norm1/2`、`{p}.attn`、`{p}.mlp.lin1/2`。`grid` 是特征图边长（全局注意力的组边长）。
    pub(crate) fn load(
        w: &Weights,
        p: &str,
        heads: usize,
        window: usize,
        grid: usize,
        eps: f32,
    ) -> Result<Layer, String> {
        let side = if window == 0 { grid } else { window };
        Ok(Layer {
            ln1: LayerNorm::load(w, &format!("{p}.layer_norm1"), eps)?,
            attn: Attention::load(w, &format!("{p}.attn"), heads, side)?,
            ln2: LayerNorm::load(w, &format!("{p}.layer_norm2"), eps)?,
            lin1: Linear::load(w, &format!("{p}.mlp.lin1"))?,
            lin2: Linear::load(w, &format!("{p}.mlp.lin2"))?,
            window,
        })
    }

    /// `x` 是 `[grid², C]`（行优先的 grid×grid 网格），返回同形状。
    pub(crate) fn forward(&self, x: Tensor, grid: usize) -> Tensor {
        let (n, c) = x.tc();
        assert_eq!(n, grid * grid, "token 数与网格不符");
        let normed = self.ln1.forward(&x);
        let attended = if self.window == 0 {
            self.attn.forward(&normed)
        } else {
            let (wins, padded) = partition(&normed.data, grid, grid, c, self.window);
            let t = Tensor::new(vec![wins.len() / c, c], wins);
            let a = self.attn.forward(&t);
            let back = unpartition(&a.data, padded, grid, grid, c, self.window);
            Tensor::new(vec![n, c], back)
        };
        let x = ops::add(&x, &attended);
        let mut hdn = self.lin1.forward(&self.ln2.forward(&x));
        // [协议] `hidden_act = "gelu"`：精确（erf）版
        Activation::Gelu.apply_all(&mut hdn);
        ops::add(&x, &self.lin2.forward(&hdn))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 窗口切分与还原可逆() {
        // 48×48 网格、窗口 14：补到 56×56，共 16 个窗口
        let (h, w, c, win) = (48, 48, 3, 14);
        let x: Vec<f32> = (0..h * w * c).map(|v| v as f32).collect();
        let (wins, (ph, pw)) = partition(&x, h, w, c, win);
        assert_eq!((ph, pw), (56, 56));
        assert_eq!(wins.len(), 16 * win * win * c);
        assert_eq!(unpartition(&wins, (ph, pw), h, w, c, win), x);
        // 补边是 0：最后一个窗口（右下）的最后一个 token 落在补边里
        assert!(wins[wins.len() - c..].iter().all(|&v| v == 0.0));
        // 第 2 个窗口（右邻）的第一个 token 是原图 (0, 14)
        let at = win * win * c;
        assert_eq!(wins[at], x[14 * c]);
        // 第 5 个窗口（第二行第一个）的第一个 token 是原图 (14, 0)
        let at = 4 * win * win * c;
        assert_eq!(wins[at], x[14 * w * c]);
    }

    #[test]
    fn 整除时不补边() {
        let (h, w, c, win) = (6, 4, 2, 2);
        let x: Vec<f32> = (0..h * w * c).map(|v| v as f32 + 1.0).collect();
        let (wins, p) = partition(&x, h, w, c, win);
        assert_eq!(p, (6, 4));
        assert!(wins.iter().all(|&v| v != 0.0));
        assert_eq!(unpartition(&wins, p, h, w, c, win), x);
    }
}
