//! 单元测试用：统计当前线程在闭包内新分配、仍存活的字节峰值，用来证明超限在分配之前被拒绝。
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

pub struct Probe;

fn note(delta: isize) {
    ACTIVE.with(|active| {
        if active.get() {
            LIVE.with(|live| {
                let v = live.get() + delta;
                live.set(v);
                PEAK.with(|peak| {
                    if v > peak.get() {
                        peak.set(v);
                    }
                });
            });
        }
    });
}

// SAFETY: 只是包一层 System，记账不碰分配出来的内存。
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            note(layout.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        note(-(layout.size() as isize));
        unsafe { System.dealloc(p, layout) }
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, new) };
        if !q.is_null() {
            note(new as isize - layout.size() as isize);
        }
        q
    }
}

pub fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ACTIVE.with(|a| a.set(true));
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    let out = f();
    ACTIVE.with(|a| a.set(false));
    (out, PEAK.with(|p| p.get().max(0) as usize))
}
