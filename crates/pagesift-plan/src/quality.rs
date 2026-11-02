//! 质量分（DESIGN §6.4）：校准过的 `P(结果完整可用)`。
//!
//! 特征来自提取结果本身（正文长度、覆盖率、结构、拦截置信、链接密度）。先用 Firth 修正的逻辑回归
//! 打分（权重总是有限，近乎可分的特征不会把预测推得过于自信），再用保序回归校准（Zadrozny & Elkan 2002）。
//! 两步分在不同的数据上拟合，校准不复用回归的训练集。

use pagesift_content::Document;
use pagesift_stat::calib::{Isotonic, Logistic};
use pagesift_stat::{DecodeError, Reader, Writer};

use crate::script::ScriptWeights;

/// 特征名，与 [`features`] 返回的向量一一对应；第一维是截距。[推导] 长度是下面列出的特征数。
pub const FEATURE_NAMES: [&str; 13] = [
    "bias",
    "ln_text_weight",
    "ln_html_bytes",
    "coverage_measured",
    "coverage_share",
    "page_share",
    "root_in_comments",
    "ln_comment_ratio",
    "barrier_confidence",
    "is_article",
    "ln_headings",
    "ln_paragraphs",
    "link_text_share",
];

pub type Features = [f64; FEATURE_NAMES.len()];

/// Markdown 的块结构计数：按行粗分，够做特征用。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Shape {
    headings: usize,
    paragraphs: usize,
    visible_chars: usize,
    link_chars: usize,
}

fn shape(md: &str) -> Shape {
    let mut s = Shape::default();
    let mut in_para = false;
    let mut in_code = false;
    for line in md.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            in_code = !in_code;
            in_para = false;
            continue;
        }
        if in_code {
            continue;
        }
        if t.is_empty() {
            in_para = false;
            continue;
        }
        let t = if let Some(rest) = t.strip_prefix('#') {
            s.headings += 1;
            in_para = false;
            rest.trim_start_matches('#')
        } else {
            if !in_para {
                s.paragraphs += 1;
                in_para = true;
            }
            t
        };
        // 链接文字：[文字](地址) 中括号里的部分；地址不算可见文字
        let mut depth = 0usize;
        let mut skip_url = false;
        let mut prev = '\0';
        for c in t.chars() {
            match c {
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                '(' if prev == ']' => skip_url = true,
                ')' if skip_url => skip_url = false,
                c if !c.is_whitespace() && !skip_url => {
                    s.visible_chars += 1;
                    if depth > 0 {
                        s.link_chars += 1;
                    }
                }
                _ => {}
            }
            prev = c;
        }
    }
    s
}

/// 一页结果的特征向量。`html_bytes` 是送去提取的 HTML 字节数。
pub fn features(doc: &Document, html_bytes: usize, weights: &ScriptWeights) -> Features {
    let sh = shape(&doc.markdown);
    let text = doc.plain_text.as_deref().unwrap_or(&doc.markdown);
    let tw = weights.text_weight(text);
    let cov = &doc.coverage;
    let out = cov.out_chars() as f64;
    let (measured, share) = match cov.main_chars() {
        Some(main) if main > 0 => (1.0, (out / main as f64).min(1.0)),
        // 整页文字少到不可能"只取到一部分"时没有量，按完整计
        _ => (0.0, 1.0),
    };
    let comments = cov.comment_chars().unwrap_or(0) as f64;
    [
        1.0,
        tw.ln_1p(),
        (html_bytes as f64).ln_1p(),
        measured,
        share,
        if cov.page_chars() == 0 {
            1.0
        } else {
            (out / cov.page_chars() as f64).min(1.0)
        },
        f64::from(u8::from(cov.root_in_comments())),
        (comments / (out + 1.0)).ln_1p(),
        doc.barrier
            .as_ref()
            .map_or(0.0, |b| f64::from(b.confidence)),
        f64::from(u8::from(doc.metadata.is_article)),
        (sh.headings as f64).ln_1p(),
        (sh.paragraphs as f64).ln_1p(),
        if sh.visible_chars == 0 {
            0.0
        } else {
            sh.link_chars as f64 / sh.visible_chars as f64
        },
    ]
}

/// 质量模型：Firth 逻辑回归打分 + 保序回归校准。交叉拟合时保留每一折的打分模型，
/// 校准曲线只用在拟合它时打分的那些模型上，预测取各模型校准后概率的平均。
#[derive(Debug, Clone, PartialEq)]
pub struct QualityModel {
    pub scores: Vec<Logistic>,
    pub calibration: Isotonic,
}

/// [协议] 模型编码的格式版本；特征定义变了就加一，旧模型整体作废
pub const MODEL_VERSION: u32 = 1;

impl QualityModel {
    /// 只拟合打分的逻辑回归。在拟合集上取值恒定的特征（除截距）不带信息，也会让正规方程奇异：
    /// 不参与拟合，权重记 0。
    fn fit_score(set: &[(Features, bool)]) -> Option<Logistic> {
        let first = set.first()?.0;
        let active: Vec<usize> = (0..FEATURE_NAMES.len())
            .filter(|&i| i == 0 || set.iter().any(|(f, _)| f[i] != first[i]))
            .collect();
        let xs: Vec<Vec<f64>> = set
            .iter()
            .map(|(f, _)| active.iter().map(|&i| f[i]).collect())
            .collect();
        let ys: Vec<bool> = set.iter().map(|(_, y)| *y).collect();
        let reduced = Logistic::fit(&xs, &ys)?;
        let mut w = vec![0.0; FEATURE_NAMES.len()];
        for (k, &i) in active.iter().enumerate() {
            w[i] = reduced.w[k];
        }
        Some(Logistic { w })
    }

    /// `fit_set` 拟合打分，`calib_set` 拟合保序校准，两者不能是同一批数据。
    pub fn fit(fit_set: &[(Features, bool)], calib_set: &[(Features, bool)]) -> Option<Self> {
        let score = Self::fit_score(fit_set)?;
        let s: Vec<f64> = calib_set.iter().map(|(f, _)| score.predict(f)).collect();
        let y: Vec<bool> = calib_set.iter().map(|(_, y)| *y).collect();
        Some(Self {
            calibration: Isotonic::fit(&s, &y),
            scores: vec![score],
        })
    }

    /// 交叉拟合（Zadrozny & Elkan 2002 §4 用留出数据校准的做法，两折互换）：`a`、`b` 轮流拟合打分并给对方打分，
    /// 校准用两份全部的样本外分数，比单独切一份校准集多用一倍数据。
    ///
    /// 两个打分模型都保留，预测时各自校准再平均（与 scikit-learn `CalibratedClassifierCV` 的 ensemble 方式相同）。
    /// 不能另用 `a ∪ b` 重新拟合一个打分模型再套这条校准曲线：数据量不同，分数的尺度不同，曲线就套错了对象。
    pub fn fit_cross(a: &[(Features, bool)], b: &[(Features, bool)]) -> Option<Self> {
        let (sa, sb) = (Self::fit_score(a)?, Self::fit_score(b)?);
        let oof = |part: &[(Features, bool)], m: &Logistic| -> (Vec<f64>, Vec<bool>) {
            part.iter().map(|(f, y)| (m.predict(f), *y)).unzip()
        };
        let (mut s, mut y) = oof(b, &sa);
        let (s_a, y_a) = oof(a, &sb);
        s.extend(s_a);
        y.extend(y_a);
        Some(Self {
            calibration: Isotonic::fit(&s, &y),
            scores: vec![sa, sb],
        })
    }

    /// 校准后的 `P(完整可用)`。
    pub fn predict(&self, f: &Features) -> f64 {
        let sum: f64 = self
            .scores
            .iter()
            .map(|m| self.calibration.predict(m.predict(f)))
            .sum();
        sum / self.scores.len() as f64
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(MODEL_VERSION);
        w.u32(FEATURE_NAMES.len() as u32);
        w.u32(self.scores.len() as u32);
        for m in &self.scores {
            m.encode(&mut w);
        }
        self.calibration.encode(&mut w);
        w.into_inner()
    }

    /// 版本或特征维数不符时返回 `None`：调用方当作没有模型。
    pub fn decode(buf: &[u8]) -> Result<Option<Self>, DecodeError> {
        let mut r = Reader::new(buf);
        if r.u32()? != MODEL_VERSION || r.u32()? as usize != FEATURE_NAMES.len() {
            return Ok(None);
        }
        let n = r.u32()? as usize;
        let scores = (0..n)
            .map(|_| Logistic::decode(&mut r))
            .collect::<Result<Vec<_>, _>>()?;
        let calibration = Isotonic::decode(&mut r)?;
        if scores.is_empty() || scores.iter().any(|m| m.w.len() != FEATURE_NAMES.len()) {
            return Err(DecodeError::Invalid("quality model dims"));
        }
        Ok(Some(Self {
            scores,
            calibration,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_shape_counts_blocks_and_link_text() {
        let s = shape(
            "# T\n\npara one\nstill one\n\n[link](http://x/y) tail\n\n```\n# not heading\n```\n",
        );
        assert_eq!((s.headings, s.paragraphs), (1, 2));
        // 可见字符：T、paraone、stillone、link、tail
        assert_eq!(s.visible_chars, 1 + 7 + 8 + 4 + 4);
        assert_eq!(s.link_chars, 4);
    }

    #[test]
    fn model_round_trip_and_monotone_output() {
        let mut data: Vec<(Features, bool)> = Vec::new();
        for i in 0..400 {
            let mut f = [0.0; FEATURE_NAMES.len()];
            f[0] = 1.0;
            f[1] = i as f64 / 40.0;
            data.push((f, (i * 7919) % 400 < i));
        }
        let (a, b) = data.split_at(200);
        let m = QualityModel::fit(a, b).unwrap();
        let mut lo = data[0].0;
        let mut hi = data[0].0;
        lo[1] = 0.0;
        hi[1] = 10.0;
        assert!(m.predict(&lo) <= m.predict(&hi));
        assert_eq!(QualityModel::decode(&m.encode()).unwrap(), Some(m));
    }
}
