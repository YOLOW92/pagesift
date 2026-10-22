//! 数学符号表、TeX 命令映射、重音与函数名词典。

use crate::font::MathKind;

pub(crate) const ACCENTS: &[(char, &str)] = &[
    ('ˆ', "\\hat"),
    ('^', "\\hat"),
    ('\u{0302}', "\\widehat"),
    ('˜', "\\tilde"),
    ('~', "\\tilde"),
    ('\u{0303}', "\\widetilde"),
    ('¯', "\\bar"),
    ('\u{0304}', "\\bar"),
    ('\u{0305}', "\\overline"),
    ('˙', "\\dot"),
    ('\u{0307}', "\\dot"),
    ('¨', "\\ddot"),
    ('\u{0308}', "\\ddot"),
    ('ˇ', "\\check"),
    ('˘', "\\breve"),
    ('´', "\\acute"),
    ('`', "\\grave"),
    ('\u{20D7}', "\\vec"),
    ('⃗', "\\vec"),
];

pub(crate) const FUNCS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "log", "ln", "lg", "exp", "lim", "liminf", "limsup",
    "max", "min", "sup", "inf", "det", "gcd", "arg", "deg", "dim", "ker", "hom", "Pr", "sinh",
    "cosh", "tanh", "coth", "arcsin", "arccos", "arctan",
];

pub(crate) fn is_greek(c: char) -> bool {
    matches!(c, 'Α'..='Ω' | 'α'..='ω' | 'ϑ' | 'ϕ' | 'ϖ' | 'ϱ' | 'ϵ' | 'ϰ')
}

pub(crate) fn is_math_symbol(c: char) -> bool {
    matches!(c as u32, 0x2200..=0x22FF | 0x27C0..=0x27FF | 0x2980..=0x2AFF | 0x1D400..=0x1D7FF)
        || matches!(
            c,
            '→' | '←'
                | '↔'
                | '⇒'
                | '⇐'
                | '⇔'
                | '↦'
                | '↪'
                | '⇀'
                | '↼'
                | '±'
                | '∓'
                | '×'
                | '÷'
                | '′'
                | '″'
                | 'ℝ'
                | 'ℕ'
                | 'ℤ'
                | 'ℚ'
                | 'ℂ'
                | 'ℓ'
                | '℘'
                | 'ℵ'
        )
}

pub(crate) fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x20D0..=0x20FF)
}

pub(crate) fn wrap(kind: MathKind, c: char) -> Option<&'static str> {
    if !c.is_ascii_alphabetic() {
        return None;
    }
    match kind {
        MathKind::Blackboard => Some("\\mathbb"),
        MathKind::Symbol if c.is_ascii_uppercase() => Some("\\mathcal"),
        MathKind::Script => Some("\\mathscr"),
        MathKind::Fraktur => Some("\\mathfrak"),
        _ => None,
    }
}

pub(crate) fn push_char(c: char, kind: MathKind, out: &mut String) {
    if is_combining(c) && c != '\u{0338}' {
        return;
    }
    if ('\u{E000}'..='\u{F8FF}').contains(&c) {
        out.push_str(match c {
            '\u{F8EB}' => "(",
            '\u{F8F6}' => ")",
            '\u{F8EE}' => "[",
            '\u{F8F9}' => "]",
            '\u{F8F1}' => "\\{",
            '\u{F8FC}' => "\\}",
            _ => "",
        });
        return;
    }
    if let Some(w) = wrap(kind, c) {
        out.push_str(&format!("{w}{{{c}}}"));
        return;
    }
    if let Some((w, base)) = math_alnum(c) {
        match w {
            Some(w) => out.push_str(&format!("{w}{{{base}}}")),
            None => push_char(base, MathKind::Text, out),
        }
        return;
    }
    let cmd = match c {
        'α' => "\\alpha",
        'β' => "\\beta",
        'γ' => "\\gamma",
        'δ' => "\\delta",
        'ϵ' => "\\epsilon",
        'ε' => "\\varepsilon",
        'ζ' => "\\zeta",
        'η' => "\\eta",
        'θ' => "\\theta",
        'ϑ' => "\\vartheta",
        'ι' => "\\iota",
        'κ' => "\\kappa",
        'ϰ' => "\\varkappa",
        'λ' => "\\lambda",
        'μ' | 'µ' => "\\mu",
        'ν' => "\\nu",
        'ξ' => "\\xi",
        'ο' => "o",
        'π' => "\\pi",
        'ϖ' => "\\varpi",
        'ρ' => "\\rho",
        'ϱ' => "\\varrho",
        'σ' => "\\sigma",
        'ς' => "\\varsigma",
        'τ' => "\\tau",
        'υ' => "\\upsilon",
        'ϕ' => "\\phi",
        'φ' => "\\varphi",
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
        'Υ' => "\\Upsilon",
        'Φ' => "\\Phi",
        'Ψ' => "\\Psi",
        'Ω' => "\\Omega",
        'Α' => "A",
        'Β' => "B",
        'Ε' => "E",
        'Ζ' => "Z",
        'Η' => "H",
        'Ι' => "I",
        'Κ' => "K",
        'Μ' => "M",
        'Ν' => "N",
        'Ο' => "O",
        'Ρ' => "P",
        'Τ' => "T",
        'Χ' => "X",
        '∑' => "\\sum",
        '∏' => "\\prod",
        '∐' => "\\coprod",
        '∫' => "\\int",
        '∬' => "\\iint",
        '∮' => "\\oint",
        '⋃' => "\\bigcup",
        '⋂' => "\\bigcap",
        '⋀' => "\\bigwedge",
        '⋁' => "\\bigvee",
        '⨁' => "\\bigoplus",
        '⨂' => "\\bigotimes",
        '⨀' => "\\bigodot",
        '⨄' => "\\biguplus",
        '⨆' => "\\bigsqcup",
        '∞' => "\\infty",
        '±' => "\\pm",
        '∓' => "\\mp",
        '×' => "\\times",
        '÷' => "\\div",
        '·' | '⋅' => "\\cdot",
        '∘' | '◦' => "\\circ",
        '∗' => "\\ast",
        '⋆' => "\\star",
        '∙' | '•' => "\\bullet",
        '⋄' => "\\diamond",
        '⊕' => "\\oplus",
        '⊖' => "\\ominus",
        '⊗' => "\\otimes",
        '⊘' => "\\oslash",
        '⊙' => "\\odot",
        '≤' => "\\leq",
        '≥' => "\\geq",
        '⩽' => "\\leqslant",
        '⩾' => "\\geqslant",
        '≠' => "\\neq",
        '≈' => "\\approx",
        '≡' => "\\equiv",
        '≅' => "\\cong",
        '∼' => "\\sim",
        '≃' => "\\simeq",
        '≍' => "\\asymp",
        '∝' => "\\propto",
        '≪' => "\\ll",
        '≫' => "\\gg",
        '≺' => "\\prec",
        '≻' => "\\succ",
        '⪯' => "\\preceq",
        '⪰' => "\\succeq",
        '≲' => "\\lesssim",
        '≳' => "\\gtrsim",
        '≐' => "\\doteq",
        '≔' => "\\coloneqq",
        '≜' => "\\triangleq",
        '≮' => "\\nless",
        '≯' => "\\ngtr",
        '≰' => "\\nleq",
        '≱' => "\\ngeq",
        '≁' => "\\nsim",
        '≢' => "\\not\\equiv",
        '→' | '⟶' => "\\to",
        '←' | '⟵' => "\\leftarrow",
        '↔' | '⟷' => "\\leftrightarrow",
        '⇒' => "\\Rightarrow",
        '⇐' => "\\Leftarrow",
        '⇔' => "\\Leftrightarrow",
        '⟹' => "\\Longrightarrow",
        '⟸' => "\\Longleftarrow",
        '⟺' => "\\Longleftrightarrow",
        '↦' => "\\mapsto",
        '⟼' => "\\longmapsto",
        '↪' => "\\hookrightarrow",
        '↩' => "\\hookleftarrow",
        '↑' => "\\uparrow",
        '↓' => "\\downarrow",
        '↕' => "\\updownarrow",
        '⇑' => "\\Uparrow",
        '⇓' => "\\Downarrow",
        '↗' => "\\nearrow",
        '↘' => "\\searrow",
        '↖' => "\\nwarrow",
        '↙' => "\\swarrow",
        '⇀' => "\\rightharpoonup",
        '↼' => "\\leftharpoonup",
        '⇁' => "\\rightharpoondown",
        '↽' => "\\leftharpoondown",
        '⇉' => "\\rightrightarrows",
        '∈' => "\\in",
        '∉' => "\\notin",
        '∋' => "\\ni",
        '∌' => "\\not\\ni",
        '⊂' => "\\subset",
        '⊃' => "\\supset",
        '⊆' => "\\subseteq",
        '⊇' => "\\supseteq",
        '⊊' => "\\subsetneq",
        '⊋' => "\\supsetneq",
        '⊄' => "\\not\\subset",
        '⊈' => "\\nsubseteq",
        '⊑' => "\\sqsubseteq",
        '⊒' => "\\sqsupseteq",
        '∪' => "\\cup",
        '∩' => "\\cap",
        '⊎' => "\\uplus",
        '⊔' => "\\sqcup",
        '⊓' => "\\sqcap",
        '∖' => "\\setminus",
        '∅' => "\\emptyset",
        '∀' => "\\forall",
        '∃' => "\\exists",
        '∄' => "\\nexists",
        '¬' => "\\neg",
        '∧' => "\\wedge",
        '∨' => "\\vee",
        '⊢' => "\\vdash",
        '⊣' => "\\dashv",
        '⊨' => "\\models",
        '⊤' => "\\top",
        '⊥' => "\\perp",
        '∂' => "\\partial",
        '∇' => "\\nabla",
        '√' => "\\surd",
        '…' => "\\ldots",
        '⋯' => "\\cdots",
        '⋮' => "\\vdots",
        '⋱' => "\\ddots",
        '′' => "'",
        '″' => "''",
        '∥' => "\\|",
        '∦' => "\\nparallel",
        '∣' => "\\mid",
        '∤' => "\\nmid",
        '⟨' | '〈' => "\\langle",
        '⟩' | '〉' => "\\rangle",
        '⌊' => "\\lfloor",
        '⌋' => "\\rfloor",
        '⌈' => "\\lceil",
        '⌉' => "\\rceil",
        'ℓ' => "\\ell",
        '℘' => "\\wp",
        'ℵ' => "\\aleph",
        'ℜ' => "\\Re",
        'ℑ' => "\\Im",
        'ℏ' => "\\hbar",
        'ı' => "\\imath",
        'ȷ' => "\\jmath",
        '△' => "\\triangle",
        '▽' => "\\triangledown",
        '◁' => "\\triangleleft",
        '▷' => "\\triangleright",
        '⊲' => "\\lhd",
        '⊳' => "\\rhd",
        '□' => "\\square",
        '■' | '∎' => "\\blacksquare",
        '♭' => "\\flat",
        '♮' => "\\natural",
        '♯' => "\\sharp",
        '∠' => "\\angle",
        '≀' => "\\wr",
        '†' => "\\dagger",
        '‡' => "\\ddagger",
        '§' => "\\S",
        '−' | '–' => "-",
        '{' => "\\{",
        '}' => "\\}",
        '\\' => "\\backslash",
        '%' => "\\%",
        '#' => "\\#",
        '&' => "\\&",
        '$' => "\\$",
        '_' => "\\_",
        '\u{0338}' => "\\not",
        'ℝ' => "\\mathbb{R}",
        'ℕ' => "\\mathbb{N}",
        'ℤ' => "\\mathbb{Z}",
        'ℚ' => "\\mathbb{Q}",
        'ℂ' => "\\mathbb{C}",
        'ℍ' => "\\mathbb{H}",
        'ℙ' => "\\mathbb{P}",
        'ℒ' => "\\mathcal{L}",
        'ℱ' => "\\mathcal{F}",
        'ℋ' => "\\mathcal{H}",
        'ℐ' => "\\mathcal{I}",
        'ℳ' => "\\mathcal{M}",
        'ℛ' => "\\mathcal{R}",
        'ℬ' => "\\mathcal{B}",
        'ℰ' => "\\mathcal{E}",
        'ℎ' => "h",
        _ => {
            out.push(c);
            return;
        }
    };
    out.push_str(cmd);
    if cmd.starts_with('\\') && cmd.chars().last().is_some_and(|c| c.is_ascii_alphabetic()) {
        out.push(' ');
    }
}

pub(crate) fn math_alnum(c: char) -> Option<(Option<&'static str>, char)> {
    let v = c as u32;
    if !(0x1D400..=0x1D7FF).contains(&v) {
        return None;
    }
    const LETTER_STYLES: [Option<&str>; 13] = [
        Some("\\mathbf"),
        None,
        Some("\\boldsymbol"),
        Some("\\mathcal"),
        Some("\\mathcal"),
        Some("\\mathfrak"),
        Some("\\mathbb"),
        Some("\\mathfrak"),
        Some("\\mathsf"),
        Some("\\mathsf"),
        Some("\\mathsf"),
        Some("\\mathsf"),
        Some("\\mathtt"),
    ];
    if v < 0x1D6A4 {
        let k = v - 0x1D400;
        let style = LETTER_STYLES[(k / 52) as usize];
        let i = k % 52;
        let base = if i < 26 {
            b'A' + i as u8
        } else {
            b'a' + (i - 26) as u8
        };
        return Some((style, base as char));
    }
    if v >= 0x1D7CE {
        let k = v - 0x1D7CE;
        let style = [
            Some("\\mathbf"),
            Some("\\mathbb"),
            Some("\\mathsf"),
            Some("\\mathsf"),
            Some("\\mathtt"),
        ][(k / 10) as usize];
        return Some((style, (b'0' + (k % 10) as u8) as char));
    }
    match v {
        0x1D6A4 => Some((None, 'ı')),
        0x1D6A5 => Some((None, 'ȷ')),
        0x1D6A8..=0x1D7C9 => {
            let k = (v - 0x1D6A8) % 58;
            let base = match k {
                0..=24 => char::from_u32(0x391 + k),
                26..=50 => char::from_u32(0x3B1 + k - 26),
                _ => None,
            }?;
            Some((None, base))
        }
        _ => None,
    }
}
