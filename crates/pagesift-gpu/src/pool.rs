//! 显存池：`cuMemAlloc` 一次要几百微秒，每层都分配会吃掉连续执行省下的时间。
//! 所有内核都排在同一条流上按序执行，所以缓冲一还回来就能给后面的内核用，不用等它们跑完。

use std::collections::HashMap;

use crate::Gpu;
use crate::buffer::DeviceBuffer;

/// 显存不够的错误都带这个前缀，调用方据此减小批次，而不是放弃 GPU。
pub const OOM_TAG: &str = "out of GPU memory";

/// 错误是否是显存不足（减小批次还有救）。
pub fn is_oom(e: &str) -> bool {
    e.starts_with(OOM_TAG)
}

/// 把元素数向上取整到容量档：每个二进制数量级分 8 档，浪费不超过 1/8，
/// 宽度不同的批次也能复用同一批缓冲。
pub(crate) fn capacity_class(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    // [推导] 每档差 2^(位数 − 4)，即 1/8 到 1/16 个数量级：8 档/倍频
    let unit = 1usize << (usize::BITS - len.leading_zeros()).saturating_sub(4);
    len.div_ceil(unit) * unit
}

/// 空闲缓冲按容量档存放；同时记录显存用量。
pub(crate) struct Pool<'g> {
    free: HashMap<usize, Vec<DeviceBuffer<'g>>>,
    /// 分配过、还没还给驱动的字节数（在用的加空闲的）。
    held: u64,
    peak: u64,
}

impl<'g> Pool<'g> {
    pub fn new() -> Pool<'g> {
        Pool {
            free: HashMap::new(),
            held: 0,
            peak: 0,
        }
    }

    /// 取一块至少 `cap` 个元素的缓冲。`cap` 必须已经是容量档。显存不够时先把空闲的还给驱动再试。
    pub fn take(&mut self, gpu: &'g Gpu, cap: usize) -> Result<DeviceBuffer<'g>, String> {
        if let Some(b) = self.free.get_mut(&cap).and_then(Vec::pop) {
            return Ok(b);
        }
        let b = match gpu.alloc(cap) {
            Ok(b) => b,
            Err(_) => {
                self.trim();
                gpu.alloc(cap)
                    .map_err(|e| format!("{OOM_TAG}: need {} bytes ({e})", bytes(cap)))?
            }
        };
        self.held += bytes(cap);
        self.peak = self.peak.max(self.held);
        Ok(b)
    }

    /// 还回来。
    pub fn give(&mut self, cap: usize, b: DeviceBuffer<'g>) {
        self.free.entry(cap).or_default().push(b);
    }

    /// 把空闲缓冲全还给驱动。
    pub fn trim(&mut self) {
        let idle: u64 = self
            .free
            .iter()
            .map(|(&cap, v)| bytes(cap) * v.len() as u64)
            .sum();
        self.free.clear();
        self.held -= idle;
    }

    pub fn peak(&self) -> u64 {
        self.peak
    }

    pub fn reset_peak(&mut self) {
        self.peak = self.held;
    }

    /// 空闲缓冲占的字节数（需要时能还给驱动）。
    pub fn idle(&self) -> u64 {
        self.free
            .iter()
            .map(|(&cap, v)| bytes(cap) * v.len() as u64)
            .sum()
    }
}

pub(crate) fn bytes(len: usize) -> u64 {
    len as u64 * size_of::<f32>() as u64
}

#[cfg(test)]
mod tests {
    use super::capacity_class;

    #[test]
    fn 容量档不小于请求且浪费有界() {
        assert_eq!(capacity_class(0), 0);
        for len in 1..100_000usize {
            let c = capacity_class(len);
            assert!(c >= len);
            assert!((c - len) * 8 <= len.max(8), "{len} → {c}");
        }
        // 相近的宽度落在同一档，才能复用
        assert_eq!(capacity_class(1000), capacity_class(1020));
    }
}
