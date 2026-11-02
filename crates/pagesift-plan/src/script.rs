//! 文字体系权重的在线估计，取代固定的 `CJK_WEIGHT = 3`（CONSTANTS.md A 类）。
//!
//! 权重的定义：该文字体系每个字符的平均信息量与拉丁字母的比值。信息量用已提取正文里
//! 该体系字符的一元分布熵估计 `Ĥ = −Σ p̂ ln p̂`，加 Miller–Madow 偏差修正 `(K−1)/(2N)`
//! （G. A. Miller, "Note on the bias of information estimates", 1955；`K` 是出现过的不同字符数，
//! `N` 是字符总数）。一元熵忽略了上下文，是信息量的上界；两个体系用同一种上界相比，比值的偏差小于各自的偏差。

use std::collections::HashMap;

use pagesift_stat::{DecodeError, Reader, Stat, Writer};

/// 文字体系。范围按 Unicode 区块表（The Unicode Standard 15.0, Blocks.txt）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Script {
    Latin,
    Han,
    Kana,
    Hangul,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Devanagari,
    Thai,
    /// 数字、标点、符号等各体系共用的字符：按定义与拉丁同权（基准）。
    Common,
}

impl Script {
    /// 全部变体。[推导] 长度是变体数。
    pub const ALL: [Script; 11] = [
        Script::Latin,
        Script::Han,
        Script::Kana,
        Script::Hangul,
        Script::Cyrillic,
        Script::Greek,
        Script::Arabic,
        Script::Hebrew,
        Script::Devanagari,
        Script::Thai,
        Script::Common,
    ];

    pub fn of(c: char) -> Script {
        match c as u32 {
            // [协议] 各区块的码位范围（Unicode Blocks.txt）
            0x41..=0x5A | 0x61..=0x7A | 0xC0..=0x24F | 0x1E00..=0x1EFF => Script::Latin,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F => Script::Han,
            0x3040..=0x30FF | 0x31F0..=0x31FF => Script::Kana,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Script::Hangul,
            0x400..=0x52F => Script::Cyrillic,
            0x370..=0x3FF | 0x1F00..=0x1FFF => Script::Greek,
            0x600..=0x6FF | 0x750..=0x77F => Script::Arabic,
            0x590..=0x5FF => Script::Hebrew,
            0x900..=0x97F => Script::Devanagari,
            0xE00..=0xE7F => Script::Thai,
            _ => Script::Common,
        }
    }
}

/// 各文字体系的字符计数；合并就是计数相加。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptStats {
    counts: HashMap<char, u64>,
}

/// 由 [`ScriptStats`] 算出的权重表。
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptWeights {
    weights: Vec<(Script, f64)>,
}

impl Default for ScriptWeights {
    /// 没有观测时各体系同权：信息不足，不假设哪种文字更"重"。
    fn default() -> Self {
        Self {
            weights: Script::ALL.iter().map(|&s| (s, 1.0)).collect(),
        }
    }
}

impl ScriptStats {
    /// 记录一段正文的字符（空白不算）。
    pub fn observe(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            *self.counts.entry(c).or_default() += 1;
        }
    }

    /// 体系 `s` 的熵估计（nats/字符）与字符总数；没有观测时为 `None`。
    pub fn entropy(&self, s: Script) -> Option<(f64, u64)> {
        let mut n = 0u64;
        let mut k = 0u64;
        let mut sum_clnc = 0.0;
        for (&c, &cnt) in &self.counts {
            if Script::of(c) == s && cnt > 0 {
                n += cnt;
                k += 1;
                sum_clnc += cnt as f64 * (cnt as f64).ln();
            }
        }
        if n == 0 {
            return None;
        }
        let nf = n as f64;
        // Ĥ = ln N − Σ c ln c / N，再加 Miller–Madow 修正 (K−1)/(2N)
        Some((nf.ln() - sum_clnc / nf + (k as f64 - 1.0) / (2.0 * nf), n))
    }

    /// 权重 = 体系熵 / 拉丁熵。拉丁或该体系没有观测时为 1（信息不足，同权）。
    /// 熵的标准误约 `√(Var(ln p)/N)`；样本少到比值不可分辨时也取 1。
    pub fn weights(&self, delta: pagesift_stat::Delta) -> ScriptWeights {
        let latin = self.entropy(Script::Latin);
        let weights = Script::ALL
            .iter()
            .map(|&s| {
                let w = match (s, latin, self.entropy(s)) {
                    (Script::Latin | Script::Common, ..) => 1.0,
                    (_, Some((hl, nl)), Some((hs, ns))) if hl > 0.0 => {
                        let ratio = hs / hl;
                        // 比值的相对标准误按两个熵各自的相对标准误合成；熵估计量的方差上界取 (ln N)²/N（Paninski 2003）
                        let rel = ((nl as f64).ln().powi(2) / nl as f64 / (hl * hl)
                            + (ns as f64).ln().powi(2) / ns as f64 / (hs * hs))
                            .sqrt();
                        if (ratio - 1.0).abs() > delta.z() * rel * ratio {
                            ratio
                        } else {
                            1.0
                        }
                    }
                    _ => 1.0,
                };
                (s, w)
            })
            .collect();
        ScriptWeights { weights }
    }
}

impl ScriptWeights {
    pub fn weight(&self, s: Script) -> f64 {
        self.weights
            .iter()
            .find(|(x, _)| *x == s)
            .map_or(1.0, |&(_, w)| w)
    }

    /// 正文的加权长度：空白不算，每个字符按所属体系的权重计。
    pub fn text_weight(&self, text: &str) -> f64 {
        let mut per = [0u64; Script::ALL.len()];
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            per[Script::ALL
                .iter()
                .position(|&s| s == Script::of(c))
                .unwrap_or(0)] += 1;
        }
        Script::ALL
            .iter()
            .zip(per)
            .map(|(&s, n)| self.weight(s) * n as f64)
            .sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Script, f64)> + '_ {
        self.weights.iter().copied()
    }
}

impl Stat for ScriptStats {
    fn merge(&mut self, o: &Self) {
        for (&c, &n) in &o.counts {
            *self.counts.entry(c).or_default() += n;
        }
    }

    fn encode(&self, w: &mut Writer) {
        let mut items: Vec<(char, u64)> = self.counts.iter().map(|(&c, &n)| (c, n)).collect();
        items.sort();
        w.u64(items.len() as u64);
        for (c, n) in items {
            w.u32(c as u32);
            w.u64(n);
        }
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let n = r.u64()?;
        let mut counts = HashMap::new();
        for _ in 0..n {
            let c = char::from_u32(r.u32()?).ok_or(DecodeError::Invalid("script char"))?;
            counts.insert(c, r.u64()?);
        }
        Ok(Self { counts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_stat::Delta;

    #[test]
    fn uniform_alphabets_give_the_log_ratio() {
        // 拉丁 26 个字母均匀出现，汉字 2600 个均匀出现：熵比 ln 2600 / ln 26 ≈ 2.41
        let mut st = ScriptStats::default();
        let latin: String = (0..26 * 400)
            .map(|i| (b'a' + (i % 26) as u8) as char)
            .collect();
        let han: String = (0..2600 * 40)
            .map(|i| char::from_u32(0x4E00 + (i % 2600) as u32).unwrap())
            .collect();
        st.observe(&latin);
        st.observe(&han);
        let w = st.weights(Delta::DEFAULT);
        let expect = 2600f64.ln() / 26f64.ln();
        assert!(
            (w.weight(Script::Han) - expect).abs() < 0.05,
            "{}",
            w.weight(Script::Han)
        );
        assert_eq!(w.weight(Script::Cyrillic), 1.0);
        assert!((w.text_weight("ab 中") - (2.0 + w.weight(Script::Han))).abs() < 1e-12);
        let mut wr = Writer::new();
        st.encode(&mut wr);
        assert_eq!(
            ScriptStats::decode(&mut Reader::new(wr.as_slice())).unwrap(),
            st
        );
    }
}
