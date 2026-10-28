//! pagesift 自己的测试共用的工具，不发布。
//!
//! 复杂度护栏（"这么大的输入必须在多少时间内处理完"）量的是被测代码用掉的 CPU 时间，不是墙钟。
//! 墙钟把别的测试、别的进程抢走的时间也算进去：CI 的 macOS 机器只有几个核，同时跑着别的测试，
//! 墙钟时有时无地翻几倍，护栏就误报。本线程的 CPU 时间只随被测代码自己的工作量变，
//! 平方级退化照样会超出预算。被测代码必须在调用线程上运行（解析、抽取、匹配都是）。
//!
//! 验证截止时间（"超时后多久返回"）的测试量的就是墙钟，不用这里的工具。

use std::time::Duration;

/// 当前线程到现在为止用掉的 CPU 时间（用户态 + 内核态）。
pub fn thread_cpu_time() -> Duration {
    imp::thread_cpu_time()
}

/// 运行 `f`，返回结果和它在当前线程上用掉的 CPU 时间。
pub fn cpu_time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = thread_cpu_time();
    let out = f();
    (out, thread_cpu_time().saturating_sub(start))
}

/// 按优化构建定的预算；未优化构建（`cargo test` 默认，CI 也是）慢一个数量级，放宽 [`DEBUG_FACTOR`] 倍。
pub fn budget(release: Duration) -> Duration {
    if cfg!(debug_assertions) {
        release * DEBUG_FACTOR
    } else {
        release
    }
}

/// 未优化构建相对优化构建的放宽倍数。
pub const DEBUG_FACTOR: u32 = 10;

#[cfg(unix)]
mod imp {
    use std::time::Duration;

    pub fn thread_cpu_time() -> Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` 是有效的可写 timespec；CLOCK_THREAD_CPUTIME_ID 在 Linux 和 macOS 上都有。
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}

#[cfg(not(unix))]
mod imp {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    /// 没有线程 CPU 时钟的平台退回墙钟。
    pub fn thread_cpu_time() -> Duration {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeping_uses_no_cpu_and_spinning_does() {
        let (_, slept) = cpu_time(|| std::thread::sleep(Duration::from_millis(200)));
        let (_, spun) = cpu_time(|| {
            let t = std::time::Instant::now();
            let mut x = 0u64;
            while t.elapsed() < Duration::from_millis(100) {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        });
        if cfg!(unix) {
            assert!(slept < Duration::from_millis(50), "{slept:?}");
        }
        assert!(spun >= Duration::from_millis(50), "{spun:?}");
    }

    #[test]
    fn other_threads_do_not_count() {
        let (_, own) = cpu_time(|| {
            std::thread::spawn(|| {
                let t = std::time::Instant::now();
                while t.elapsed() < Duration::from_millis(150) {
                    std::hint::black_box(0);
                }
            })
            .join()
            .unwrap();
        });
        if cfg!(unix) {
            assert!(own < Duration::from_millis(50), "{own:?}");
        }
    }
}
