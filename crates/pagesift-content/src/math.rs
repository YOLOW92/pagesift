//! 网页里的数学公式转成 LaTeX（Markdown 里写成 `$…$` 或 `$$…$$`）。
//!
//! 能拿到原始 TeX 时直接用：KaTeX 和维基百科在 MathML 里附带 `<annotation encoding="application/x-tex">`，
//! MathJax 2 把源码放在 `<script type="math/tex">` 里，维基百科的兜底图片 `alt` 也是 TeX。
//! 只有 MathML 时（MathJax 3 的辅助 MathML、手写的 `<math>`）用自研的 MathML → LaTeX 转换。
//! 渲染给人看的那份重复内容（KaTeX 的 `katex-html`、MathJax 的字形 span、预览）整段跳过，
//! 否则正文里会多出一串拆散的字形。

use pagesift_html::Tag;

use crate::dom::{NodeR, element, raw_text};

/// 一个公式节点的处理方式。
pub(crate) enum Math {
    /// 公式：TeX 源码和是否独立显示。
    Tex(String, bool),
    /// 公式的视觉副本或预览，不输出。
    Skip,
}

/// 源码里有没有可能出现公式：`<math>`、`math/tex`、`katex-mathml`、`mwe-math` 和 MathJax 的脚本地址都含小写的
/// `math`，MathJax 3 的输出是 `mjx-`。绝大多数页面没有，渲染时就不用对每个元素做公式检查。
pub(crate) fn may_have_math(source: &str) -> bool {
    let b = source.as_bytes();
    memchr::memmem::find(b, b"math").is_some() || memchr::memmem::find(b, b"mjx-").is_some()
}

fn has_class(class: &str, name: &str) -> bool {
    class.split_ascii_whitespace().any(|c| c == name)
}

/// 这个节点是不是公式（或公式的视觉副本）。只看标签和 class，开销很小，可以对每个元素调用。
pub(crate) fn math_of(node: &NodeR<'_>) -> Option<Math> {
    let el = element(node)?;
    match el.tag() {
        Tag::Math => {
            let display = el
                .attr("display")
                .is_some_and(|d| d.eq_ignore_ascii_case("block"));
            Some(Math::Tex(tex_of_math(node)?, display))
        }
        Tag::Script => {
            let ty = el.attr("type")?.to_ascii_lowercase();
            if !ty.starts_with("math/tex") {
                return None;
            }
            let tex = raw_text(node).trim().to_string();
            (!tex.is_empty()).then(|| Math::Tex(tex, ty.contains("mode=display")))
        }
        Tag::Span | Tag::Div => {
            let class = el.attr("class")?;
            if !(class.contains("katex")
                || class.contains("math")
                || class.contains("Math")
                || class.contains("MJX"))
            {
                return None;
            }
            if has_class(class, "katex-display") || has_class(class, "katex") {
                let display = has_class(class, "katex-display");
                return Some(match find_math(node) {
                    Some(m) => Math::Tex(tex_of_math(&m)?, display),
                    None => Math::Skip,
                });
            }
            if has_class(class, "mwe-math-element") {
                // 维基百科：隐藏的 MathML（带 TeX 标注）加一张兜底图片
                let display = node.descendants().filter_map(|d| element(&d)).any(|e| {
                    e.attr("class").is_some_and(|c| {
                        c.contains("mwe-math-mathml-display")
                            || c.contains("mwe-math-fallback-image-display")
                    })
                });
                let tex = find_math(node).and_then(|m| tex_of_math(&m)).or_else(|| {
                    node.descendants()
                        .filter_map(|d| element(&d))
                        .find(|e| e.tag() == Tag::Img)
                        .and_then(|e| e.attr("alt"))
                        .map(strip_displaystyle)
                })?;
                return Some(Math::Tex(tex, display));
            }
            if [
                "MathJax_Preview",
                "MathJax",
                "MathJax_Display",
                "MathJax_SVG",
                "MathJax_SVG_Display",
                "MathJax_CHTML",
                "MJX_Assistive_MathML",
            ]
            .iter()
            .any(|c| has_class(class, c))
            {
                return Some(Math::Skip);
            }
            None
        }
        Tag::Other if el.name() == "mjx-container" => {
            let display = el
                .attr("display")
                .is_some_and(|d| d == "true" || d == "block");
            Some(match find_math(node).and_then(|m| tex_of_math(&m)) {
                Some(t) => Math::Tex(t, display),
                None => Math::Skip,
            })
        }
        _ => None,
    }
}

fn find_math<'a>(node: &NodeR<'a>) -> Option<NodeR<'a>> {
    node.descendants().find(|d| d.tag() == Some(Tag::Math))
}

/// 规范化 TeX：去掉维基百科 alt 的外层 `{\displaystyle …}`，去掉结尾的空格命令（LaTeXML 常在末尾留下
/// `\ `、`\,`），否则紧贴着的 `$` 会被转义成 `\$`。
fn strip_displaystyle(s: &str) -> String {
    let t = s.trim();
    let mut t = t
        .strip_prefix("{\\displaystyle")
        .and_then(|r| r.strip_suffix('}'))
        .map(|r| r.trim().to_string())
        .unwrap_or_else(|| t.to_string());
    loop {
        let before = t.len();
        for sp in ["\\,", "\\;", "\\!", "\\:", "\\quad", "\\qquad"] {
            if t.ends_with(sp) {
                t.truncate(t.len() - sp.len());
            }
        }
        // 奇数个结尾反斜杠：最后一个是 `\ ` 空格命令去掉空格后剩下的
        let slashes = t.bytes().rev().take_while(|&b| b == b'\\').count();
        if slashes % 2 == 1 {
            t.pop();
        }
        t.truncate(t.trim_end().len());
        if t.len() == before {
            return t;
        }
    }
}

/// `<math>` 的 TeX：先找 TeX 标注，再看 `alttext`，最后从 MathML 结构转换。
fn tex_of_math(math: &NodeR<'_>) -> Option<String> {
    for d in math.descendants() {
        if let Some(e) = element(&d)
            && e.name() == "annotation"
            && e.attr("encoding")
                .is_some_and(|enc| enc.eq_ignore_ascii_case("application/x-tex"))
        {
            let t = raw_text(&d).trim().to_string();
            if !t.is_empty() {
                return Some(strip_displaystyle(&t));
            }
        }
    }
    if let Some(alt) = element(math)
        .and_then(|e| e.attr("alttext"))
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        return Some(strip_displaystyle(alt));
    }
    let mut out = String::new();
    mathml(math, &mut out);
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!out.is_empty()).then_some(out)
}

/// 子元素（跳过文本空白）。
fn kids<'a>(node: &NodeR<'a>) -> Vec<NodeR<'a>> {
    node.children().filter(|c| c.is_element()).collect()
}

fn group(node: &NodeR<'_>) -> String {
    let mut s = String::new();
    mathml(node, &mut s);
    let s = s.trim().to_string();
    let command =
        s.len() > 1 && s.starts_with('\\') && s[1..].chars().all(|c| c.is_ascii_alphabetic());
    if s.chars().count() == 1 || command {
        s
    } else {
        format!("{{{s}}}")
    }
}

/// MathML → LaTeX（常用元素；其余按内容拼接）。
fn mathml(node: &NodeR<'_>, out: &mut String) {
    let Some(el) = element(node) else {
        if let Some(t) = node.as_text() {
            out.push_str(t.trim());
        }
        return;
    };
    let k = kids(node);
    match el.name() {
        "annotation" | "annotation-xml" | "mphantom" => {}
        "semantics" => {
            if let Some(first) = k.first() {
                mathml(first, out);
            }
        }
        "mi" | "mn" => {
            let t = raw_text(node).trim().to_string();
            if el.name() == "mi" && t.chars().count() > 1 && FUNCS.contains(&t.as_str()) {
                out.push('\\');
                out.push_str(&t);
                out.push(' ');
            } else {
                for c in t.chars() {
                    push_symbol(c, out);
                }
            }
        }
        "mo" => {
            let t = raw_text(node).trim().to_string();
            for c in t.chars() {
                push_symbol(c, out);
            }
            out.push(' ');
        }
        "mtext" => {
            let t = raw_text(node).trim().to_string();
            if !t.is_empty() {
                out.push_str(&format!("\\text{{{t}}}"));
            }
        }
        "mspace" => out.push(' '),
        "msup" if k.len() == 2 => out.push_str(&format!("{}^{}", group(&k[0]), group(&k[1]))),
        "msub" if k.len() == 2 => out.push_str(&format!("{}_{}", group(&k[0]), group(&k[1]))),
        "msubsup" if k.len() == 3 => out.push_str(&format!(
            "{}_{}^{}",
            group(&k[0]),
            group(&k[1]),
            group(&k[2])
        )),
        "munder" if k.len() == 2 => out.push_str(&format!("{}_{}", group(&k[0]), group(&k[1]))),
        "mover" if k.len() == 2 => out.push_str(&format!("{}^{}", group(&k[0]), group(&k[1]))),
        "munderover" if k.len() == 3 => out.push_str(&format!(
            "{}_{}^{}",
            group(&k[0]),
            group(&k[1]),
            group(&k[2])
        )),
        "mfrac" if k.len() == 2 => {
            let mut a = String::new();
            let mut b = String::new();
            mathml(&k[0], &mut a);
            mathml(&k[1], &mut b);
            out.push_str(&format!("\\frac{{{}}}{{{}}}", a.trim(), b.trim()));
        }
        "msqrt" => {
            let mut a = String::new();
            for c in &k {
                mathml(c, &mut a);
            }
            out.push_str(&format!("\\sqrt{{{}}}", a.trim()));
        }
        "mroot" if k.len() == 2 => {
            let mut a = String::new();
            let mut b = String::new();
            mathml(&k[0], &mut a);
            mathml(&k[1], &mut b);
            out.push_str(&format!("\\sqrt[{}]{{{}}}", b.trim(), a.trim()));
        }
        "mfenced" => {
            out.push_str(el.attr("open").unwrap_or("("));
            for (i, c) in k.iter().enumerate() {
                if i > 0 {
                    out.push_str(
                        el.attr("separators")
                            .and_then(|s| s.trim().chars().next())
                            .map_or(",", |_| ","),
                    );
                }
                mathml(c, out);
            }
            out.push_str(el.attr("close").unwrap_or(")"));
        }
        "mtable" => {
            out.push_str("\\begin{matrix} ");
            for (i, row) in k.iter().enumerate() {
                if i > 0 {
                    out.push_str(" \\\\ ");
                }
                for (j, cell) in kids(row).iter().enumerate() {
                    if j > 0 {
                        out.push_str(" & ");
                    }
                    mathml(cell, out);
                }
            }
            out.push_str(" \\end{matrix}");
        }
        _ => {
            for c in node.children() {
                mathml(&c, out);
            }
        }
    }
}

const FUNCS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "log", "ln", "exp", "lim", "max", "min", "sup",
    "inf", "det", "gcd", "arg", "deg", "dim", "ker", "Pr", "sinh", "cosh", "tanh", "arcsin",
    "arccos", "arctan",
];

/// Unicode 数学符号 → TeX 命令；普通字符原样。
fn push_symbol(c: char, out: &mut String) {
    let cmd = match c {
        'α' => "\\alpha",
        'β' => "\\beta",
        'γ' => "\\gamma",
        'δ' => "\\delta",
        'ε' | 'ϵ' => "\\epsilon",
        'ζ' => "\\zeta",
        'η' => "\\eta",
        'θ' => "\\theta",
        'ι' => "\\iota",
        'κ' => "\\kappa",
        'λ' => "\\lambda",
        'μ' => "\\mu",
        'ν' => "\\nu",
        'ξ' => "\\xi",
        'π' => "\\pi",
        'ρ' => "\\rho",
        'σ' => "\\sigma",
        'τ' => "\\tau",
        'υ' => "\\upsilon",
        'φ' | 'ϕ' => "\\phi",
        'χ' => "\\chi",
        'ψ' => "\\psi",
        'ω' => "\\omega",
        'Γ' => "\\Gamma",
        'Δ' => "\\Delta",
        'Θ' => "\\Theta",
        'Λ' => "\\Lambda",
        'Ξ' => "\\Xi",
        'Π' => "\\Pi",
        'Σ' => "\\Sigma",
        'Φ' => "\\Phi",
        'Ψ' => "\\Psi",
        'Ω' => "\\Omega",
        '∑' => "\\sum",
        '∏' => "\\prod",
        '∫' => "\\int",
        '∮' => "\\oint",
        '∞' => "\\infty",
        '±' => "\\pm",
        '∓' => "\\mp",
        '×' => "\\times",
        '÷' => "\\div",
        '·' | '⋅' => "\\cdot",
        '≤' => "\\le",
        '≥' => "\\ge",
        '≠' => "\\ne",
        '≈' => "\\approx",
        '≡' => "\\equiv",
        '∼' => "\\sim",
        '∝' => "\\propto",
        '→' => "\\to",
        '←' => "\\leftarrow",
        '⇒' => "\\Rightarrow",
        '⇔' => "\\Leftrightarrow",
        '↦' => "\\mapsto",
        '∈' => "\\in",
        '∉' => "\\notin",
        '⊂' => "\\subset",
        '⊆' => "\\subseteq",
        '∪' => "\\cup",
        '∩' => "\\cap",
        '∅' => "\\emptyset",
        '∀' => "\\forall",
        '∃' => "\\exists",
        '¬' => "\\neg",
        '∧' => "\\land",
        '∨' => "\\lor",
        '∂' => "\\partial",
        '∇' => "\\nabla",
        '√' => "\\sqrt",
        '…' => "\\ldots",
        '⋯' => "\\cdots",
        '′' => "'",
        '−' => "-",
        '\u{2061}' | '\u{2062}' | '\u{2063}' => "",
        _ => {
            out.push(c);
            return;
        }
    };
    out.push_str(cmd);
    if cmd.starts_with('\\') {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagesift_html::Html;

    fn first_math(html: &str) -> Option<(String, bool)> {
        let dom = Html::parse(html);
        dom.root().descendants().find_map(|n| match math_of(&n) {
            Some(Math::Tex(t, d)) => Some((t, d)),
            _ => None,
        })
    }

    #[test]
    fn reads_tex_from_katex_mathjax_and_wikipedia() {
        let katex = r#"<span class="katex-display"><span class="katex"><span class="katex-mathml"><math><semantics><mrow><mi>x</mi></mrow><annotation encoding="application/x-tex">x^2 + 1</annotation></semantics></math></span><span class="katex-html" aria-hidden="true">x2+1</span></span></span>"#;
        assert_eq!(first_math(katex), Some(("x^2 + 1".into(), true)));
        let mj = r#"<span class="MathJax_Preview"></span><span class="MathJax">x</span><script type="math/tex">\alpha</script>"#;
        assert_eq!(first_math(mj), Some((r"\alpha".into(), false)));
        let wiki = r#"<span class="mwe-math-element"><span class="mwe-math-mathml-inline mwe-math-mathml-a11y" style="display: none;"><math alttext="{\displaystyle E=mc^{2}}"><semantics><mrow/><annotation encoding="application/x-tex">{\displaystyle E=mc^{2}}</annotation></semantics></math></span><img class="mwe-math-fallback-image-inline" alt="{\displaystyle E=mc^{2}}"></span>"#;
        assert_eq!(first_math(wiki), Some(("E=mc^{2}".into(), false)));
        let latexml = r#"<math alttext="x\in\{a\}\\ y\end{cases}\ "><mi>x</mi></math>"#;
        assert_eq!(
            first_math(latexml),
            Some((r"x\in\{a\}\\ y\end{cases}".into(), false))
        );
    }

    #[test]
    fn converts_plain_mathml() {
        let m = r#"<math display="block"><mfrac><mrow><mi>a</mi><mo>+</mo><mn>1</mn></mrow><msup><mi>b</mi><mn>2</mn></msup></mfrac><mo>≤</mo><msqrt><mi>π</mi></msqrt><mo>,</mo><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover><mi>sin</mi><mi>x</mi></math>"#;
        let (t, d) = first_math(m).unwrap();
        assert!(d);
        assert_eq!(t, r"\frac{a+ 1}{b^2}\le \sqrt{\pi}, \sum_{i= 1}^n\sin x");
    }
}
