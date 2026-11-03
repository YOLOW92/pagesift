//! 注意力用的两个多线程小工具（`std::thread::scope`，现开现收）。pagesift-infer 的线程池工具
//! 是 crate 内部的，这里不去改它，单独写一份最小的。

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 机器可用的线程数。
fn max_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// 对 `0..n` 每项调 `f`，结果按下标排好返回。各线程用原子计数动态领取。
pub(crate) fn map<R: Send>(n: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let threads = max_threads().min(n);
    if threads <= 1 {
        return (0..n).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(n));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut mine = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    mine.push((i, f(i)));
                }
                done.lock().unwrap_or_else(|e| e.into_inner()).extend(mine);
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|e| e.into_inner());
    done.sort_unstable_by_key(|&(i, _)| i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// 把 `data` 按 `chunk` 个元素切段，连续若干段分给一个线程；`f(段号, 段)`。
pub(crate) fn chunks_mut(data: &mut [f32], chunk: usize, f: impl Fn(usize, &mut [f32]) + Sync) {
    let chunk = chunk.max(1);
    let parts = data.len().div_ceil(chunk);
    let threads = max_threads().min(parts);
    if threads <= 1 {
        for (i, s) in data.chunks_mut(chunk).enumerate() {
            f(i, s);
        }
        return;
    }
    let per = parts.div_ceil(threads);
    std::thread::scope(|s| {
        for (gi, group) in data.chunks_mut(per * chunk).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (k, part) in group.chunks_mut(chunk).enumerate() {
                    f(gi * per + k, part);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_keeps_order() {
        let v = map(100, |i| i * i);
        assert!(v.iter().enumerate().all(|(i, &x)| x == i * i));
        assert!(map(0, |i| i).is_empty());
    }

    #[test]
    fn chunks_cover_everything_once() {
        let mut d = vec![0f32; 1003];
        chunks_mut(&mut d, 10, |i, s| {
            for v in s {
                *v += i as f32 + 1.0;
            }
        });
        assert!(
            d.iter()
                .enumerate()
                .all(|(j, &v)| v == (j / 10) as f32 + 1.0)
        );
    }
}
