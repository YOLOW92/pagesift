//! 按页质量分与低分页复核。
//!
//! 解释每页时顺带数三样证据（`geom::TextEvidence`）：
//! - 乱码率：解出的字里 U+FFFD、私用区、控制字符的比例（坏的 ToUnicode 常把字映射到这些码位）；
//! - 字体映射缺失：字体给不出 Unicode、只能丢掉的字形，占所有字形的比例；
//! - 阅读顺序跳变：内容流里换行时往回跳（同一栏里跳到更高的行）的次数占换行次数的比例。
//!
//! 门槛不写死，用 δ 规则：一页的“缺字率”（乱码 + 映射缺失）的 Wilson 下界
//! 超过 δ，即有 1−δ 的把握这页坏掉的字多于 δ，才判为低分页；页面一个字也没有、却有被略过的
//! 按钮表单域时也复核（这类页唯一的字在按钮上）。低分页只重做这一页：换一条路再解释一遍
//! （见 [`crate::interp::Interp::alternate`]），哪条路得到的好字多就用哪条。
//!
//! 阅读顺序跳变只记分、不触发重做：版面重组本来就不按内容流顺序排字，跳变高的页说明写入方
//! 的顺序不可信，正是版面重组该管的；而本仓库的评测集没有阅读顺序的标注，换一种排法是否
//! 更好无从判定（见 docs/history/0.88_pdf.md）。

use pagesift_stat::Delta;
use pagesift_stat::beta::wilson;

use crate::geom::TextEvidence;

/// 乱码字符：替换字符 U+FFFD、基本平面私用区 U+E000–U+F8FF、C0/C1 控制字符
/// （Unicode 15 §23.1、§23.5；空白在调用前已去掉）。
pub(crate) fn garbled(c: char) -> bool {
    // [协议] Unicode 码位：U+FFFD、私用区 E000–F8FF、C0 00–1F、DEL 与 C1 7F–9F
    matches!(c as u32, 0xFFFD | 0xE000..=0xF8FF | 0x00..=0x1F | 0x7F..=0x9F)
}

/// 一页的质量分。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PageQuality {
    /// 解出的字符数（不含空白）。
    pub chars: usize,
    /// 其中的乱码字符数。
    pub garbled: usize,
    /// 字体给不出 Unicode、被丢掉的字形数。
    pub unmapped: usize,
    /// 内容流里的换行次数。
    pub breaks: usize,
    /// 其中往回跳的次数。
    pub jumps: usize,
    /// 这页被判为低分页并换路径重做过。
    pub rechecked: bool,
    /// 重做的结果被采用了。
    pub replaced: bool,
}

impl PageQuality {
    pub(crate) fn from_evidence(ev: &TextEvidence) -> PageQuality {
        PageQuality {
            chars: ev.chars,
            garbled: ev.garbled,
            unmapped: ev.unmapped,
            breaks: ev.breaks,
            jumps: ev.jumps,
            rechecked: false,
            replaced: false,
        }
    }

    /// 缺字率：(乱码 + 映射缺失) / (解出的字 + 映射缺失)。没有字时为 None。
    pub fn defect_rate(&self) -> Option<f64> {
        let n = self.chars + self.unmapped;
        (n > 0).then(|| (self.garbled + self.unmapped) as f64 / n as f64)
    }

    /// 阅读顺序跳变率：跳变 / 换行。没有换行时为 None。
    pub fn jump_rate(&self) -> Option<f64> {
        (self.breaks > 0).then(|| self.jumps as f64 / self.breaks as f64)
    }

    /// 质量分 ∈ [0, 1]，越高越好：(1 − 缺字率)·(1 − 跳变率)，缺的项按 1 算；一个字也没有时为 None。
    pub fn score(&self) -> Option<f64> {
        let text = 1.0 - self.defect_rate()?;
        Some(text * (1.0 - self.jump_rate().unwrap_or(0.0)))
    }

    /// 好字数：解出的字减去乱码。复核时两条路比这个。
    pub(crate) fn good(&self) -> usize {
        self.chars - self.garbled
    }
}

/// 为什么要复核。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recheck {
    /// 缺字率显著高于 δ。
    Defects,
    /// 没有字，但有按钮表单域。
    OnlyButtons,
}

/// δ 规则：缺字数 `k`、字形数 `n` 的 Wilson 下界（单侧 1−δ）超过 δ 才复核。
pub(crate) fn needs_recheck(ev: &TextEvidence, delta: Delta) -> Option<Recheck> {
    let n = ev.chars + ev.unmapped;
    if n == 0 {
        return (ev.buttons > 0).then_some(Recheck::OnlyButtons);
    }
    let k = ev.garbled + ev.unmapped;
    let (lo, _) = wilson(k as f64, n as f64, delta);
    (lo > delta.get()).then_some(Recheck::Defects)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(chars: usize, garbled: usize, unmapped: usize) -> TextEvidence {
        TextEvidence {
            chars,
            garbled,
            unmapped,
            ..TextEvidence::default()
        }
    }

    #[test]
    fn garbled_classes() {
        for c in ['\u{fffd}', '\u{e000}', '\u{f8e6}', '\u{1}', '\u{85}'] {
            assert!(garbled(c), "{c:?}");
        }
        for c in ['a', 'é', 'τ', '中', '∑', '\u{f900}'] {
            assert!(!garbled(c), "{c:?}");
        }
    }

    /// δ 规则：缺字率明显超过 δ 才复核；样本少时同样的比例不够显著。
    #[test]
    fn recheck_needs_significance() {
        let d = Delta::DEFAULT;
        // 2000 个字里 1% 坏：Wilson 下界远低于 0.05
        assert_eq!(needs_recheck(&ev(2000, 20, 0), d), None);
        // 2000 个字里 30% 坏
        assert_eq!(needs_recheck(&ev(2000, 600, 0), d), Some(Recheck::Defects));
        // 映射缺失同样算
        assert_eq!(needs_recheck(&ev(1000, 0, 300), d), Some(Recheck::Defects));
        // 10 个字里坏 1 个（10%）：下界不过 δ
        assert_eq!(needs_recheck(&ev(10, 1, 0), d), None);
        // 空页：有按钮才复核
        assert_eq!(needs_recheck(&ev(0, 0, 0), d), None);
        let mut e = ev(0, 0, 0);
        e.buttons = 2;
        assert_eq!(needs_recheck(&e, d), Some(Recheck::OnlyButtons));
    }

    #[test]
    fn score_combines_text_and_order() {
        let mut q = PageQuality::from_evidence(&ev(90, 9, 10));
        assert_eq!(q.defect_rate(), Some(0.19));
        assert_eq!(q.score(), Some(0.81));
        q.breaks = 10;
        q.jumps = 5;
        assert!((q.score().unwrap() - 0.405).abs() < 1e-12);
        assert_eq!(PageQuality::default().score(), None);
        assert_eq!(q.good(), 81);
    }
}
