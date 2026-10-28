//! 两个测试程序共用的压测：每块写入特征字节，释放和扩缩容前校验内容没被别处改过。

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc};
use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Block {
    ptr: *mut u8,
    layout: Layout,
    tag: u8,
}

// SAFETY: 测试里一块只被一个线程持有，转手后原线程不再碰它。
unsafe impl Send for Block {}

fn layout(rng: &mut Rng) -> Layout {
    let size = match rng.below(10) {
        0..=6 => rng.below(513),
        7 | 8 => 513 + rng.below(4096),
        _ => rng.below(70_000),
    } as usize;
    let align = 1usize << rng.below(7);
    Layout::from_size_align(size.max(1), align).unwrap()
}

fn fill(b: &Block) {
    // SAFETY: ptr 指向 layout.size() 字节的可写内存。
    unsafe { std::ptr::write_bytes(b.ptr, b.tag, b.layout.size()) };
}

fn check(b: &Block, len: usize) {
    // SAFETY: 同上，len ≤ size。
    let s = unsafe { std::slice::from_raw_parts(b.ptr, len) };
    assert!(s.iter().all(|&x| x == b.tag), "block corrupted");
    assert_eq!(b.ptr as usize % b.layout.align(), 0, "misaligned");
}

fn new_block(rng: &mut Rng) -> Block {
    let layout = layout(rng);
    let zeroed = rng.below(4) == 0;
    // SAFETY: layout 大小非零。
    let ptr = unsafe {
        if zeroed {
            alloc_zeroed(layout)
        } else {
            alloc(layout)
        }
    };
    assert!(!ptr.is_null());
    if zeroed {
        // SAFETY: 刚分配的 size 字节。
        let s = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
        assert!(s.iter().all(|&x| x == 0), "alloc_zeroed not zeroed");
    }
    let b = Block {
        ptr,
        layout,
        tag: rng.below(256) as u8,
    };
    fill(&b);
    b
}

fn free(b: Block) {
    check(&b, b.layout.size());
    // SAFETY: 按分配时的 layout 释放。
    unsafe { dealloc(b.ptr, b.layout) };
}

fn churn(seed: u64, rounds: usize) {
    let mut rng = Rng(seed | 1);
    let mut live: Vec<Block> = Vec::new();
    for _ in 0..rounds {
        match rng.below(10) {
            0..=4 => live.push(new_block(&mut rng)),
            5..=7 if !live.is_empty() => {
                let i = rng.below(live.len() as u64) as usize;
                free(live.swap_remove(i));
            }
            8 | 9 if !live.is_empty() => {
                let i = rng.below(live.len() as u64) as usize;
                let b = &mut live[i];
                check(b, b.layout.size());
                let new_size = (layout(&mut rng).size()).max(1);
                // SAFETY: 按原 layout 扩缩容，new_size 非零。
                let p = unsafe { realloc(b.ptr, b.layout, new_size) };
                assert!(!p.is_null());
                let kept = b.layout.size().min(new_size);
                b.ptr = p;
                b.layout = Layout::from_size_align(new_size, b.layout.align()).unwrap();
                check(b, kept);
                fill(b);
            }
            _ => {}
        }
    }
    for b in live {
        free(b);
    }
}

#[test]
fn many_threads_allocate_free_and_resize_at_random() {
    super::setup();
    let handles: Vec<_> = (0..8)
        .map(|t| thread::spawn(move || churn(0x9e37_79b9 * (t + 1), 200_000)))
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn blocks_freed_on_other_threads_stay_intact() {
    super::setup();
    let (tx, rx) = mpsc::sync_channel::<Block>(1024);
    let producers: Vec<_> = (0..4)
        .map(|t| {
            let tx = tx.clone();
            thread::spawn(move || {
                let mut rng = Rng(0xabcd_ef01 + t);
                for _ in 0..50_000 {
                    tx.send(new_block(&mut rng)).unwrap();
                }
            })
        })
        .collect();
    drop(tx);
    let consumer = thread::spawn(move || {
        let mut n = 0;
        for b in rx {
            free(b);
            n += 1;
        }
        n
    });
    for p in producers {
        p.join().unwrap();
    }
    assert_eq!(consumer.join().unwrap(), 200_000);
}

#[test]
fn short_lived_threads_hand_their_cache_back() {
    super::setup();
    for round in 0..50 {
        let handles: Vec<_> = (0..8)
            .map(|t| thread::spawn(move || churn(round * 31 + t + 7, 3_000)))
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
    churn(12345, 50_000);
}

#[test]
fn standard_collections_work_under_threads() {
    super::setup();
    let handles: Vec<_> = (0..8)
        .map(|t| {
            thread::spawn(move || {
                let mut m: HashMap<String, Vec<u32>> = HashMap::new();
                for i in 0..20_000u32 {
                    let k = format!("key-{}-{}", t, i % 997);
                    m.entry(k).or_default().push(i);
                    if i % 7 == 0 {
                        m.remove(&format!("key-{}-{}", t, (i / 7) % 997));
                    }
                }
                let total: usize = m.values().map(Vec::len).sum();
                let s: String = (0..1000)
                    .map(|i| char::from(b'a' + (i % 26) as u8))
                    .collect();
                assert_eq!(s.len(), 1000);
                total
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap() > 0);
    }
}
