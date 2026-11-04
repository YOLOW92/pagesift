//! 二值位图（每像素一字节，1 是黑、0 是白，JBIG2 的约定，T.88 §4.2）和区域合成运算（§6.1、§7.4.8.5）。
//!
//! 解码一个码流里分配的位图总字节数有上限（防解压炸弹：几十字节的段头就能声明 2^32 × 2^32 的区域），
//! 由 [`super::decode`] 开始时设定，所有 [`Bitmap::new`] 共用。

use std::cell::Cell;

thread_local! {
    /// 这次解码还能分配的字节数。
    static BUDGET: Cell<usize> = const { Cell::new(0) };
}

/// 设定本线程接下来的分配上限。
pub(super) fn set_budget(bytes: usize) {
    BUDGET.with(|b| b.set(bytes));
}

/// 从上限里扣掉 `bytes`（位图之外的大块分配，如上下文表，也走这里）。
pub(super) fn charge(bytes: usize) -> Result<(), String> {
    BUDGET.with(|b| {
        let left = b.get();
        if bytes > left {
            return Err(format!("要 {bytes} 字节，超过剩余的分配上限 {left} 字节"));
        }
        b.set(left - bytes);
        Ok(())
    })
}

/// 过了本次处理的截止时间就报错：分配上限管不住时间（一个半色调区域能把一张大图样合成上亿次），
/// 解码循环按段、行查这里。
pub(super) fn check_time() -> Result<(), String> {
    if crate::budget::expired() {
        return Err("解码超过了时限".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Bitmap {
    pub(super) w: usize,
    pub(super) h: usize,
    /// 按行存放，`w * h` 个字节，每个 0 或 1。
    pub(super) data: Vec<u8>,
}

/// 区域合成运算（T.88 §7.4.8.5 表 39 外部合成运算、§7.4.1.5 区域段信息标志的低 3 位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Op {
    Or,
    And,
    Xor,
    Xnor,
    Replace,
}

impl Op {
    pub(super) fn from_bits(v: u8) -> Result<Op, String> {
        Ok(match v {
            // [协议] 0 OR、1 AND、2 XOR、3 XNOR、4 REPLACE（T.88 §7.4.1.5）
            0 => Op::Or,
            1 => Op::And,
            2 => Op::Xor,
            3 => Op::Xnor,
            4 => Op::Replace,
            _ => return Err(format!("无效的合成运算 {v}")),
        })
    }

    #[inline]
    fn apply(self, dst: u8, src: u8) -> u8 {
        match self {
            Op::Or => dst | src,
            Op::And => dst & src,
            Op::Xor => dst ^ src,
            Op::Xnor => 1 ^ (dst ^ src),
            Op::Replace => src,
        }
    }
}

impl Bitmap {
    /// 全部填 `fill`（0 或 1）。超过本次解码的分配上限时报错。
    pub(super) fn new(w: usize, h: usize, fill: u8) -> Result<Bitmap, String> {
        let n = w.checked_mul(h).ok_or("位图尺寸溢出")?;
        // 宽或高为 0 的位图按 1 计：不占内存，但逐行逐列的循环也要受上限约束
        // （宽 0、高 10 亿行的区域能让解码器空转 10 亿次）
        let cost = w.max(1).checked_mul(h.max(1)).ok_or("位图尺寸溢出")?;
        charge(cost).map_err(|e| format!("{w}×{h} 的位图：{e}"))?;
        Ok(Bitmap {
            w,
            h,
            data: vec![fill; n],
        })
    }

    /// 取像素；区域外是 0（T.88 §6.2.5.2：模板取到图外的像素当 0）。
    #[inline]
    pub(super) fn get(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return 0;
        }
        self.data[y as usize * self.w + x as usize]
    }

    #[inline]
    pub(super) fn set(&mut self, x: usize, y: usize, v: u8) {
        self.data[y * self.w + x] = v;
    }

    pub(super) fn row(&self, y: usize) -> &[u8] {
        &self.data[y * self.w..(y + 1) * self.w]
    }

    pub(super) fn row_mut(&mut self, y: usize) -> &mut [u8] {
        &mut self.data[y * self.w..(y + 1) * self.w]
    }

    /// 把 `src` 以左上角 (x, y) 合成到本图上，超出本图的部分丢掉。
    pub(super) fn compose(&mut self, src: &Bitmap, x: i64, y: i64, op: Op) {
        let x0 = x.max(0);
        let x1 = (x + src.w as i64).min(self.w as i64);
        let y0 = y.max(0);
        let y1 = (y + src.h as i64).min(self.h as i64);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for dy in y0..y1 {
            let s = &src.row((dy - y) as usize)[(x0 - x) as usize..(x1 - x) as usize];
            let d = &mut self.row_mut(dy as usize)[x0 as usize..x1 as usize];
            for (d, &s) in d.iter_mut().zip(s) {
                *d = op.apply(*d, s);
            }
        }
    }

    /// 取出 (x, y) 起 w×h 的一块；超出本图的部分是 0。
    pub(super) fn crop(&self, x: i64, y: i64, w: usize, h: usize) -> Result<Bitmap, String> {
        let mut out = Bitmap::new(w, h, 0)?;
        out.compose(self, -x, -y, Op::Replace);
        Ok(out)
    }

    /// 在底部加行，用 `fill` 填（页高未知时随条带增长，T.88 §7.4.8.2）。
    pub(super) fn grow(&mut self, h: usize, fill: u8) -> Result<(), String> {
        if h <= self.h {
            return Ok(());
        }
        let more = Bitmap::new(self.w, h - self.h, fill)?;
        self.data.extend_from_slice(&more.data);
        self.h = h;
        Ok(())
    }
}
