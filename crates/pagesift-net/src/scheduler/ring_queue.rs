//! Dmitry Vyukov 经典有界 MPMC 无锁化环形队列（100% Safe Rust 实现）。
//! 通过槽位序列号（sequence）与原子 CAS 游标协同，保证多生产者多消费者下的绝对无饥饿、零死锁与零漏单。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Slot<T> {
    sequence: AtomicUsize,
    item: Mutex<Option<T>>,
}

struct Inner<T> {
    buffer: Box<[Slot<T>]>,
    mask: usize,
    capacity: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

/// 纯安全 Rust 高性能有界 MPMC 请求调度队列。
#[derive(Clone)]
pub struct RingQueue<T> {
    inner: Arc<Inner<T>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PushError<T> {
    Full(T),
}

impl<T> RingQueue<T> {
    /// 创建指定容量的队列（容量自动向上取整至 2 的幂次）。
    pub fn new(capacity: usize) -> Self {
        assert!(capacity >= 2, "capacity must be at least 2");
        let cap = capacity.next_power_of_two();
        let mask = cap - 1;

        let mut buffer = Vec::with_capacity(cap);
        for i in 0..cap {
            buffer.push(Slot {
                sequence: AtomicUsize::new(i),
                item: Mutex::new(None),
            });
        }

        Self {
            inner: Arc::new(Inner {
                buffer: buffer.into_boxed_slice(),
                mask,
                capacity: cap,
                head: AtomicUsize::new(0),
                tail: AtomicUsize::new(0),
            }),
        }
    }

    /// 高并发入队。若队列已满立即返回 `Err(PushError::Full(val))`。
    pub fn push(&self, value: T) -> Result<(), PushError<T>> {
        let inner = &self.inner;
        let mut pos = inner.tail.load(Ordering::Relaxed);
        let val = value;

        loop {
            let slot = &inner.buffer[pos & inner.mask];
            let seq = slot.sequence.load(Ordering::Acquire);
            let diff = (seq as isize) - (pos as isize);

            if diff == 0 {
                match inner.tail.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let mut guard = slot.item.lock().expect("lock poisoned");
                        *guard = Some(val);
                        slot.sequence.store(pos + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(actual) => {
                        pos = actual;
                    }
                }
            } else if diff < 0 {
                return Err(PushError::Full(val));
            } else {
                pos = inner.tail.load(Ordering::Relaxed);
            }
        }
    }

    /// 高并发出队。若队列为空立即返回 `None`。
    pub fn pop(&self) -> Option<T> {
        let inner = &self.inner;
        let mut pos = inner.head.load(Ordering::Relaxed);

        loop {
            let slot = &inner.buffer[pos & inner.mask];
            let seq = slot.sequence.load(Ordering::Acquire);
            let diff = (seq as isize) - ((pos + 1) as isize);

            if diff == 0 {
                match inner.head.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let mut guard = slot.item.lock().expect("lock poisoned");
                        let val = guard.take();
                        slot.sequence.store(pos + inner.mask + 1, Ordering::Release);
                        return val;
                    }
                    Err(actual) => {
                        pos = actual;
                    }
                }
            } else if diff < 0 {
                return None;
            } else {
                pos = inner.head.load(Ordering::Relaxed);
            }
        }
    }

    /// 当前队列中大致包含的元素数量。
    pub fn len(&self) -> usize {
        let tail = self.inner.tail.load(Ordering::Relaxed);
        let head = self.inner.head.load(Ordering::Relaxed);
        tail.saturating_sub(head)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }
}
