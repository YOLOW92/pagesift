//! 数学公式解析与 LaTeX 重构引擎。
//! 包含数学字体识别、TeX 命令映射、上下标多级分层树与行内/行间公式渲染。

pub(crate) mod footnote;
pub(crate) mod symbols;
pub(crate) mod tex;

pub(crate) use tex::{Item, line_text};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::MathKind;
    use crate::interp::Glyph;
    use symbols::math_alnum;

    fn glyph(text: &str, x: f64, y: f64, size: f64, math: MathKind) -> Glyph {
        Glyph {
            text: text.into(),
            x,
            y,
            x_end: x + 0.5 * size * text.chars().count() as f64,
            size,
            bold: false,
            math,
            mcid: None,
            artifact: false,
        }
    }

    fn line(words: &[&[Glyph]]) -> Option<String> {
        let words: Vec<Vec<Item>> = words
            .iter()
            .map(|w| {
                w.iter()
                    .map(|g| Item {
                        g,
                        text: g.text.clone(),
                    })
                    .collect()
            })
            .collect();
        line_text(&words, 100.0, 10.0)
    }

    use MathKind::{Italic, Symbol, Text};

    #[test]
    fn superscript_and_relations() {
        let w = [
            vec![glyph("for", 0.0, 100.0, 10.0, Text)],
            vec![glyph("0", 20.0, 100.0, 10.0, Text)],
            vec![glyph("≤", 30.0, 100.0, 10.0, Symbol)],
            vec![glyph("k", 40.0, 100.0, 10.0, Italic)],
            vec![glyph("≤", 50.0, 100.0, 10.0, Symbol)],
            vec![
                glyph("2", 60.0, 100.0, 10.0, Text),
                glyph("N", 65.0, 103.6, 7.0, Italic),
            ],
            vec![
                glyph("−", 72.0, 100.0, 10.0, Symbol),
                glyph("1", 78.0, 100.0, 10.0, Text),
                glyph(",", 83.0, 100.0, 10.0, Italic),
            ],
            vec![glyph("we", 90.0, 100.0, 10.0, Text)],
        ];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs).unwrap(), "for $0 \\leq k \\leq 2^{N} -1$, we");
    }

    #[test]
    fn subscripts_nested_and_fonts() {
        let w = [vec![
            glyph("x", 0.0, 100.0, 10.0, Italic),
            glyph("i", 5.0, 98.5, 7.0, Italic),
            glyph("j", 8.0, 97.5, 5.0, Italic),
            glyph("∈", 15.0, 100.0, 10.0, Symbol),
            glyph("R", 25.0, 100.0, 10.0, MathKind::Blackboard),
            glyph("F", 32.0, 100.0, 10.0, Symbol),
        ]];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(
            line(&refs).unwrap(),
            "$x_{i_{j}}\\in \\mathbb{R}\\mathcal{F}$"
        );
    }

    #[test]
    fn plain_text_is_untouched() {
        let w = [
            vec![glyph("Price", 0.0, 100.0, 10.0, Text)],
            vec![glyph("→", 30.0, 100.0, 10.0, Text)],
            vec![glyph("2019", 40.0, 100.0, 10.0, Text)],
        ];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs), None);
    }

    #[test]
    fn primes_and_accents() {
        let w = [vec![
            glyph("f", 0.0, 100.0, 10.0, Italic),
            glyph("′", 5.0, 103.0, 7.0, Symbol),
            glyph("ˆ", 10.0, 100.0, 10.0, Text),
            glyph("x", 10.0, 100.0, 10.0, Italic),
        ]];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs).unwrap(), "$f'\\hat{x}$");
    }

    #[test]
    fn function_names() {
        let w = [vec![
            glyph("c", 0.0, 100.0, 10.0, Text),
            glyph("o", 5.0, 100.0, 10.0, Text),
            glyph("s", 10.0, 100.0, 10.0, Text),
            glyph("θ", 16.0, 100.0, 10.0, Italic),
        ]];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs).unwrap(), "$\\cos \\theta$");
    }

    #[test]
    fn repeated_scripts_get_a_new_base() {
        let w = [vec![
            glyph("t", 0.0, 100.0, 10.0, Italic),
            glyph("2", 5.0, 103.6, 7.0, Text),
            glyph("1", 8.0, 98.5, 7.0, Text),
            glyph("n", 11.0, 103.6, 7.0, Italic),
            glyph("S", 20.0, 100.0, 10.0, Italic),
            glyph("i", 25.0, 98.5, 7.0, Italic),
            glyph("′", 28.0, 103.0, 7.0, Symbol),
            glyph("j", 31.0, 98.5, 7.0, Italic),
        ]];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs).unwrap(), "$t^{2}_{1}{}^{n}S_{i}'{}_{j}$");
    }

    /// 作者名后面上标的星号、剑号是脚注引用（arXiv 1706.03762 的作者行），不是公式。
    #[test]
    fn footnote_markers_after_names_are_not_math() {
        let w = [
            vec![glyph("Ashish", 0.0, 100.0, 10.0, Text)],
            vec![
                glyph("Vaswani", 35.0, 100.0, 10.0, Text),
                glyph("∗", 70.0, 103.6, 7.0, Symbol),
            ],
            vec![glyph("Noam", 80.0, 100.0, 10.0, Text)],
            vec![
                glyph("Shazeer", 105.0, 100.0, 10.0, Text),
                glyph("†", 140.0, 103.6, 7.0, Symbol),
            ],
            // 标记和名字之间有空格
            vec![glyph("Niki", 150.0, 100.0, 10.0, Text)],
            vec![glyph("Parmar", 175.0, 100.0, 10.0, Text)],
            vec![glyph("∗", 210.0, 103.6, 7.0, Symbol)],
        ];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(
            line(&refs).unwrap(),
            "Ashish Vaswani* Noam Shazeer† Niki Parmar *"
        );
    }

    /// 同样的符号在基线上、和公式字体的变量连在一起时仍是公式。
    #[test]
    fn baseline_asterisk_and_math_superscripts_stay_math() {
        let w = [vec![
            glyph("x", 0.0, 100.0, 10.0, Italic),
            glyph("∗", 5.0, 100.0, 10.0, Symbol),
            glyph("y", 12.0, 100.0, 10.0, Italic),
        ]];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs).unwrap(), "$x\\ast y$");
        let w = [vec![
            glyph("f", 0.0, 100.0, 10.0, Italic),
            glyph("∗", 5.0, 103.6, 7.0, Symbol),
        ]];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert_eq!(line(&refs).unwrap(), "$f^{\\ast}$");
        // 单独的上标数字跟在文字后面可能是指数，照旧
        let w = [
            vec![glyph("size", 0.0, 100.0, 10.0, Text)],
            vec![glyph("2", 25.0, 103.6, 7.0, Symbol)],
        ];
        let refs: Vec<&[Glyph]> = w.iter().map(|v| v.as_slice()).collect();
        assert!(line(&refs).unwrap().contains("^{2}"));
    }

    #[test]
    fn math_alphanumerics() {
        assert_eq!(math_alnum('𝑥'), Some((None, 'x')));
        assert_eq!(math_alnum('𝔽'), Some((Some("\\mathbb"), 'F')));
        assert_eq!(math_alnum('𝟏'), Some((Some("\\mathbf"), '1')));
    }
}
