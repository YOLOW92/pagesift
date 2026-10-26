//! SIMD 极速切分与滑动窗口 Token 预算生成。
//! 目标单核吞吐冲刺 >= 500 MB/s。

use super::tokenizer_cache::collect_tokens;
use std::ops::Range;

/// 生成 Token 窗口切块区间。
/// `tail` 为 true 时对应 Crawl4AI 滑动窗口行为：最后不对齐时补一个以结尾为止的整窗。
pub fn windows(
    text: &str,
    window: usize,
    step: usize,
    tail: bool,
) -> (Vec<Range<usize>>, Vec<usize>) {
    let (starts, ends) = collect_tokens(text);
    let n = starts.len();
    if n == 0 {
        return (Vec::new(), starts);
    }
    if n <= window {
        let all = 0..text.len();
        return (vec![all], starts);
    }

    let span = |a: usize, b: usize| starts[a]..ends[b - 1];
    let mut out = Vec::with_capacity((n / step) + 2);
    let mut i = 0;

    if tail {
        while i + window <= n {
            out.push(span(i, i + window));
            i += step;
        }
        if i - step + window < n {
            out.push(span(n - window, n));
        }
    } else {
        loop {
            let end = (i + window).min(n);
            out.push(span(i, end));
            if end >= n {
                break;
            }
            i += step;
        }
    }

    (out, starts)
}

/// 快速固定词长切分 (Fixed Tokens)。
#[inline]
pub fn fixed_tokens(text: &str, size: usize) -> (Vec<Range<usize>>, Vec<usize>) {
    windows(text, size, size, false)
}

/// 快速滑动窗口切分 (Sliding Window)。
#[inline]
pub fn sliding_window(text: &str, window: usize, step: usize) -> (Vec<Range<usize>>, Vec<usize>) {
    windows(text, window, step, true)
}

/// 快速重叠窗口切分 (Overlapping Window)。
#[inline]
pub fn overlapping_window(
    text: &str,
    window: usize,
    overlap: usize,
) -> (Vec<Range<usize>>, Vec<usize>) {
    let step = window - overlap;
    windows(text, window, step, false)
}
