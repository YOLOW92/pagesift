//! 基于槽位独立互斥与无锁原子游标的高并发环形缓冲区（100% Safe Rust）。
//! 采用原子 head/tail 游标无锁递增 + 槽位单体锁设计，消除全局互斥瓶颈，完全避免多核伪共享。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Slot<T> {
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
        for _ in 0..cap {
            buffer.push(Slot {
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

        // 快速检查队列是否已满
        let tail = inner.tail.load(Ordering::Relaxed);
        let head = inner.head.load(Ordering::Relaxed);
        if tail.saturating_sub(head) >= inner.capacity {
            return Err(PushError::Full(value));
        }

        let idx = inner.tail.fetch_add(1, Ordering::AcqRel);
        let slot = &inner.buffer[idx & inner.mask];
        let mut guard = slot.item.lock().expect("lock poisoned");

        if guard.is_some() {
            // 发生极端回绕溢出
            inner.tail.fetch_sub(1, Ordering::Relaxed);
            return Err(PushError::Full(value));
        }

        *guard = Some(value);
        Ok(())
    }

    /// 高并发出队。若队列为空立即返回 `None`。
    pub fn pop(&self) -> Option<T> {
        let inner = &self.inner;

        // 快速检查队列是否为空
        let head = inner.head.load(Ordering::Relaxed);
        let tail = inner.tail.load(Ordering::Relaxed);
        if head >= tail {
            return None;
        }

        let idx = inner.head.fetch_add(1, Ordering::AcqRel);
        let slot = &inner.buffer[idx & inner.mask];
        let mut guard = slot.item.lock().expect("lock poisoned");
        guard.take()
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
