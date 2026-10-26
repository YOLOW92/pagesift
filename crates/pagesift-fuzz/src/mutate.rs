//! 变异：从种子语料里挑一个输入，叠加 1～8 次随机变异。每次迭代的随机数只由
//! （基础种子，迭代编号）决定，所以任何一次迭代都能用 `--replay` 原样重放。

/// xorshift64*，够快、可复现。
pub struct Rng(u64);

impl Rng {
    pub fn for_iteration(base: u64, iter: u64) -> Rng {
        // splitmix64 把（种子，编号）打散成初始状态，相邻编号的序列互不相关
        let mut z = base ^ iter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

const INTERESTING: &[u8] = &[
    0, 0x7f, 0x80, 0xff, b'<', b'>', b'&', b'"', b'\'', b'/', b'\n', b'\r', b' ',
];

pub struct Mutator<'a> {
    pub corpus: &'a [Vec<u8>],
    pub dict: &'a [&'a [u8]],
    pub max_len: usize,
}

impl Mutator<'_> {
    pub fn generate(&self, rng: &mut Rng) -> Vec<u8> {
        let mut data = self.corpus[rng.below(self.corpus.len())].clone();
        // 偶尔原样跑种子（确认语料本身没问题）
        if rng.chance(64) {
            return data;
        }
        let rounds = 1 + rng.below(8);
        for _ in 0..rounds {
            self.mutate_once(&mut data, rng);
        }
        data.truncate(self.max_len);
        data
    }

    fn mutate_once(&self, d: &mut Vec<u8>, rng: &mut Rng) {
        let len = d.len();
        match rng.below(12) {
            // 翻转若干位
            0 if len > 0 => {
                for _ in 0..1 + rng.below(8) {
                    let i = rng.below(len);
                    d[i] ^= 1 << rng.below(8);
                }
            }
            // 某个字节换成边界值或特殊字符
            1 if len > 0 => {
                let i = rng.below(len);
                d[i] = INTERESTING[rng.below(INTERESTING.len())];
            }
            // 删一段
            2 if len > 0 => {
                let i = rng.below(len);
                let j = (i + 1 + rng.below(4096.min(len))).min(len);
                d.drain(i..j);
            }
            // 截断
            3 if len > 1 => d.truncate(rng.below(len)),
            // 复制一段插到别处
            4 if len > 0 => {
                let i = rng.below(len);
                let j = (i + 1 + rng.below(1024)).min(len);
                let piece = d[i..j].to_vec();
                let at = rng.below(len + 1);
                d.splice(at..at, piece);
            }
            // 从另一个种子拼一段进来
            5 => {
                let other = &self.corpus[rng.below(self.corpus.len())];
                if !other.is_empty() {
                    let i = rng.below(other.len());
                    let j = (i + 1 + rng.below(8192)).min(other.len());
                    let at = rng.below(len + 1);
                    d.splice(at..at, other[i..j].iter().copied());
                }
            }
            // 插入字典里的词（标签、关键字）
            6..=8 if !self.dict.is_empty() => {
                let tok = self.dict[rng.below(self.dict.len())];
                let at = rng.below(len + 1);
                d.splice(at..at, tok.iter().copied());
            }
            // 把一个词重复很多次：深层嵌套、超长属性、海量节点
            9 if !self.dict.is_empty() => {
                let tok = self.dict[rng.below(self.dict.len())];
                let reps = match rng.below(3) {
                    0 => 1 + rng.below(64),
                    1 => 1 + rng.below(4096),
                    _ => 1 + rng.below(100_000),
                };
                let reps = reps.min(self.max_len / tok.len().max(1));
                let at = rng.below(len + 1);
                let rep: Vec<u8> = tok.iter().copied().cycle().take(tok.len() * reps).collect();
                d.splice(at..at, rep);
            }
            // 插入随机字节
            10 => {
                let n = 1 + rng.below(32);
                let at = rng.below(len + 1);
                let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                d.splice(at..at, bytes);
            }
            // 一段字节整体换成同一个值
            _ if len > 0 => {
                let i = rng.below(len);
                let j = (i + 1 + rng.below(256)).min(len);
                let v = INTERESTING[rng.below(INTERESTING.len())];
                d[i..j].fill(v);
            }
            _ => d.push(rng.next() as u8),
        }
    }
}
