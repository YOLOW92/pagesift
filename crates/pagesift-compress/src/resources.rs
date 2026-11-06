//! 编码工作集随输入、等级和硬件缓存变化；协议限制不是性能调参上限。
use std::sync::OnceLock;

pub(crate) struct Budget {
    pub tagged: bool,
    pub hash_bits: u32,
    pub window: usize,
    pub depth: usize,
    pub local_span: usize,
}

#[derive(Default)]
struct Hardware {
    cache: Option<usize>,
    local_cache: Option<usize>,
    near_cache: Option<usize>,
    near_total_cache: Option<usize>,
    memory: Option<usize>,
    cpus: usize,
}

fn number(s: &str) -> Option<usize> {
    let s = s.trim();
    let (digits, scale) = match s.as_bytes().last()? {
        b'K' | b'k' => (&s[..s.len() - 1], 1024),
        b'M' | b'm' => (&s[..s.len() - 1], 1024 * 1024),
        _ => (s, 1),
    };
    digits.parse::<usize>().ok()?.checked_mul(scale)
}

fn cpu_count(s: &str) -> Option<usize> {
    s.trim().split(',').try_fold(0usize, |n, part| {
        let count = if let Some((a, b)) = part.split_once('-') {
            b.parse::<usize>()
                .ok()?
                .checked_sub(a.parse().ok()?)?
                .checked_add(1)?
        } else {
            part.parse::<usize>().ok()?;
            1
        };
        n.checked_add(count)
    })
}

impl Hardware {
    fn detect() -> Self {
        let mut hw = Self {
            cpus: std::thread::available_parallelism().map_or(1, usize::from),
            ..Self::default()
        };
        let mut nearest_shared = usize::MAX;
        // 只读硬件信息，不改变亲和性。共享缓存按逻辑 CPU 分摊，避免假定整个 LLC 独占。
        if let Ok(entries) = std::fs::read_dir("/sys/devices/system/cpu/cpu0/cache") {
            for entry in entries.flatten() {
                let p = entry.path();
                let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
                if !matches!(kind.trim(), "Data" | "Unified") {
                    continue;
                }
                let size = std::fs::read_to_string(p.join("size"))
                    .ok()
                    .and_then(|s| number(&s));
                let shared = std::fs::read_to_string(p.join("shared_cpu_list"))
                    .ok()
                    .and_then(|s| cpu_count(&s));
                if let (Some(size), Some(shared)) = (size, shared.filter(|&n| n > 0)) {
                    hw.cache = Some(hw.cache.unwrap_or(0).max(size / shared));
                    hw.local_cache = Some(hw.local_cache.unwrap_or(usize::MAX).min(size / shared));
                    if shared < nearest_shared {
                        nearest_shared = shared;
                        hw.near_cache = Some(size / shared);
                        hw.near_total_cache = Some(size);
                    } else if shared == nearest_shared {
                        hw.near_cache = Some(hw.near_cache.unwrap_or(0).max(size / shared));
                        hw.near_total_cache = Some(hw.near_total_cache.unwrap_or(0).max(size));
                    }
                }
            }
        }
        hw.memory = std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
            s.lines().find_map(|line| {
                let rest = line.strip_prefix("MemTotal:")?;
                rest.split_whitespace()
                    .next()?
                    .parse::<usize>()
                    .ok()?
                    .checked_mul(1024)
            })
        });
        // 容器里物理内存可能不属于当前进程，采用可见的更小限额。
        for path in [
            "/sys/fs/cgroup/memory.max",
            "/sys/fs/cgroup/memory/memory.limit_in_bytes",
        ] {
            if let Some(limit) = std::fs::read_to_string(path).ok().and_then(|s| number(&s)) {
                hw.memory = Some(hw.memory.unwrap_or(limit).min(limit));
            }
        }
        hw
    }
}

fn floor_power(n: usize) -> usize {
    1usize << (usize::BITS - 1 - n.max(1).leading_zeros())
}

fn hardware() -> &'static Hardware {
    static HW: OnceLock<Hardware> = OnceLock::new();
    HW.get_or_init(Hardware::detect)
}

pub(crate) fn huffman_lookup_bits(max_bits: u32) -> u32 {
    hardware().local_cache.map_or(max_bits, |cache| {
        floor_power(cache / std::mem::size_of::<u32>())
            .trailing_zeros()
            .max(max_bits)
            .min(max_bits * 2)
    })
}

impl Budget {
    pub fn new(len: usize, level: i32) -> Self {
        Self::for_hardware(len, level, hardware())
    }

    fn for_hardware(len: usize, level: i32, hw: &Hardware) -> Self {
        let word = std::mem::size_of::<usize>();
        let effort = level.max(1) as usize;
        // 信息不可用时按输入大小退化，不假装知道机器的缓存或内存。
        let cache = hw.cache.unwrap_or(len.max(word));
        let tables = if level >= 3 { 2 } else { 1 };
        let tag_bytes = crate::lz::entry_bytes(len > u32::MAX as usize);
        let tagged_fits = hw.near_total_cache.is_some_and(|cache| {
            floor_power(len)
                .saturating_mul(tables)
                .saturating_mul(tag_bytes)
                <= cache
        });
        // 元数据本身可留在近端缓存，或原文已超出近端缓存时，才使用标签过滤。
        let tagged = hw.near_cache.is_some_and(|near| len > near || tagged_fits);
        let memory = hw.memory.map_or_else(
            || len.saturating_mul(word).saturating_mul(2),
            |n| n / hw.cpus.max(1) / word,
        );
        // 标签表用更大的缓存份额换掉远端原文读取；链表还需为历史环留出缓存。
        let hash_bytes = cache
            .saturating_mul(effort)
            .saturating_div(if level < 9 && tagged { 3 } else { 6 })
            .min(memory / 2);
        let position_bytes = if len <= u32::MAX as usize { 4 } else { word };
        let entry_bytes = if level < 9 && tagged {
            crate::lz::entry_bytes(len > u32::MAX as usize)
        } else {
            position_bytes
        };
        let slots = floor_power((hash_bytes / (tables * entry_bytes)).min(len.max(1)));
        // 一份预算留给链、一份给哈希头及其他编码缓冲。偏移额外值须装进 u32。
        let positions = (memory / position_bytes / 2)
            .max(1)
            .min((u32::MAX - 3) as usize);
        let wanted = len.max(1).checked_next_power_of_two().unwrap_or(usize::MAX);
        let window = floor_power(wanted.min(positions));
        Self {
            tagged,
            hash_bits: slots.trailing_zeros(),
            window,
            depth: effort
                .saturating_mul(effort)
                .saturating_div(3)
                .max(1)
                .min(len.max(1)),
            local_span: hw.local_cache.unwrap_or(cache).max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_and_input_scale_resources() {
        let small = Hardware {
            cache: Some(256 << 10),
            local_cache: Some(16 << 10),
            near_cache: Some(128 << 10),
            near_total_cache: Some(256 << 10),
            memory: Some(1 << 30),
            cpus: 4,
        };
        let large = Hardware {
            cache: Some(4 << 20),
            local_cache: Some(64 << 10),
            near_cache: Some(1 << 20),
            near_total_cache: Some(4 << 20),
            memory: Some(usize::try_from(16u64 << 30).unwrap_or(usize::MAX)),
            cpus: 4,
        };
        let a = Budget::for_hardware(16 << 20, 9, &small);
        let b = Budget::for_hardware(16 << 20, 9, &large);
        assert!(b.hash_bits > a.hash_bits);
        assert!(b.window >= a.window);
        assert!(b.window > 65536);
        assert!(Budget::for_hardware(16 << 20, 19, &large).depth > b.depth);
        assert_eq!(Budget::for_hardware(0, 3, &large).hash_bits, 0);
        assert!(Budget::for_hardware(7, 3, &large).hash_bits <= 2);
        assert!(!Budget::for_hardware(large.near_cache.unwrap(), 3, &large).tagged);
        assert!(Budget::for_hardware(large.near_cache.unwrap() + 1, 3, &large).tagged);
        assert!(Budget::for_hardware(16 << 10, 3, &small).tagged);
        assert!(!Budget::for_hardware(64 << 10, 3, &small).tagged);
    }

    #[test]
    fn parse_hardware_values() {
        assert_eq!(number("2048K\n"), Some(2 << 20));
        assert_eq!(number("max"), None);
        assert_eq!(cpu_count("0-3,8-11\n"), Some(8));
        assert_eq!(cpu_count("4-2"), None);
    }

    #[test]
    fn missing_tiny_and_extreme_resources() {
        for hw in [
            Hardware::default(),
            Hardware {
                cache: Some(1),
                local_cache: Some(1),
                near_cache: Some(1),
                near_total_cache: Some(1),
                memory: Some(1),
                cpus: usize::MAX,
            },
        ] {
            for len in [0, 1, 7, 131073, usize::MAX] {
                for level in [i32::MIN, 3, 9, 19, i32::MAX] {
                    let b = Budget::for_hardware(len, level, &hw);
                    assert!(b.hash_bits < usize::BITS);
                    assert!(b.window.is_power_of_two());
                    assert!(b.window <= (u32::MAX - 3) as usize);
                    assert!(b.depth > 0 && b.depth <= len.max(1));
                    assert!(b.local_span > 0);
                }
            }
        }
    }
}
