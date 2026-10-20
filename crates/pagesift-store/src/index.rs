//! 内存索引：键的 64 位哈希 -> 记录位置。
//!
//! 索引不保存键本身，每个条目固定 24 字节（哈希 8 + 位置 16）。命中后读盘上的记录核对键；
//! 两个不同的键哈希相同时，后来的那个连同完整键放进 `overflow`。

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher, RandomState};
use std::sync::atomic::{AtomicU32, Ordering};

const LEN_BITS: u32 = 28;
const LEN_MASK: u32 = (1 << LEN_BITS) - 1;

pub(crate) struct Loc {
    pub seg: u32,
    pub off: u32,
    len_ns: u32,
    /// 最近访问时间（Unix 秒），LRU 用。读路径只持有读锁，所以用原子量。
    atime: AtomicU32,
}

impl Loc {
    pub fn new(seg: u32, off: u32, len: u32, ns: u8, atime: u32) -> Self {
        debug_assert!(len <= LEN_MASK && ns < 16);
        Self {
            seg,
            off,
            len_ns: len | (u32::from(ns) << LEN_BITS),
            atime: AtomicU32::new(atime),
        }
    }
    pub fn len(&self) -> u32 {
        self.len_ns & LEN_MASK
    }
    pub fn ns(&self) -> u8 {
        (self.len_ns >> LEN_BITS) as u8
    }
    pub fn atime(&self) -> u32 {
        self.atime.load(Ordering::Relaxed)
    }
    pub fn touch(&self, now: u32) {
        self.atime.store(now, Ordering::Relaxed);
    }
}

/// 哈希已经算好，直接透传。
#[derive(Default)]
pub(crate) struct PassThrough(u64);

impl Hasher for PassThrough {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("only u64 keys")
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Slot {
    Main(u64),
    Overflow,
}

pub(crate) struct Index {
    pub main: HashMap<u64, Loc, BuildHasherDefault<PassThrough>>,
    /// 键是 `[ns] + key`。
    pub overflow: HashMap<Box<[u8]>, Loc>,
    hasher: RandomState,
    mask: u64,
}

impl Index {
    pub fn new(mask: u64) -> Self {
        Self {
            main: HashMap::default(),
            overflow: HashMap::new(),
            hasher: RandomState::new(),
            mask,
        }
    }

    pub fn hash(&self, ns: u8, key: &[u8]) -> u64 {
        self.hasher.hash_one((ns, key)) & self.mask
    }

    pub fn overflow_key(ns: u8, key: &[u8]) -> Box<[u8]> {
        let mut k = Vec::with_capacity(key.len() + 1);
        k.push(ns);
        k.extend_from_slice(key);
        k.into_boxed_slice()
    }

    /// 候选位置：overflow 命中是确定的（第三项为 true），main 命中还要核对盘上的键。
    pub fn candidate(&self, ns: u8, key: &[u8]) -> Option<(Slot, &Loc, bool)> {
        if !self.overflow.is_empty()
            && let Some(loc) = self.overflow.get(&Self::overflow_key(ns, key))
        {
            return Some((Slot::Overflow, loc, true));
        }
        let h = self.hash(ns, key);
        self.main.get(&h).map(|loc| (Slot::Main(h), loc, false))
    }

    pub fn get_mut(&mut self, slot: &Slot, ns: u8, key: &[u8]) -> Option<&mut Loc> {
        match slot {
            Slot::Main(h) => self.main.get_mut(h),
            Slot::Overflow => self.overflow.get_mut(&Self::overflow_key(ns, key)),
        }
    }

    pub fn remove(&mut self, slot: &Slot, ns: u8, key: &[u8]) -> Option<Loc> {
        match slot {
            Slot::Main(h) => self.main.remove(h),
            Slot::Overflow => self.overflow.remove(&Self::overflow_key(ns, key)),
        }
    }

    /// 插入一个索引里还没有的键。
    pub fn insert_new(&mut self, ns: u8, key: &[u8], loc: Loc) {
        let h = self.hash(ns, key);
        match self.main.entry(h) {
            Entry::Occupied(_) => {
                self.overflow.insert(Self::overflow_key(ns, key), loc);
            }
            Entry::Vacant(e) => {
                e.insert(loc);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.main.len() + self.overflow.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Loc> {
        self.main.values().chain(self.overflow.values())
    }

    /// 估算堆内存：hashbrown 的桶数是容量 * 8/7 向上取到 2 的幂，每个桶多 1 字节控制位。
    pub fn heap_bytes(&self) -> u64 {
        fn table<K, V, S>(m: &HashMap<K, V, S>) -> u64 {
            if m.capacity() == 0 {
                return 0;
            }
            let buckets = (m.capacity() * 8 / 7).next_power_of_two() as u64;
            buckets * (std::mem::size_of::<(K, V)>() as u64 + 1)
        }
        let keys: u64 = self.overflow.keys().map(|k| k.len() as u64).sum();
        table(&self.main) + table(&self.overflow) + keys
    }
}
