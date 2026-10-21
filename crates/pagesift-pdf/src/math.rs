//! 行内公式还原成 LaTeX（自研）：PDF 里的公式只剩一个个字形，靠字体（TeX 的 CMMI、CMSY、CMEX、
//! MSBM 等数学字体，Cambria Math、STIX）认出公式，靠字号和基线认出上下标，靠位置重叠认出重音，
//! 再把 Unicode 符号换成 TeX 命令，写成 `$…$`。
//!
//! 一行先按空格切成词；含数学字体字形或数学符号的词是公式词，数字、括号、运算符这类词夹在公式词
//! 之间时一并算进公式。连续的公式词合成一段公式，段首段尾的标点留在公式外面。

use crate::font::MathKind;
use crate::interp::Glyph;

/// 一个字形和它规范化后的文字。
pub(crate) struct Item<'a> {
    pub(crate) g: &'a Glyph,
    pub(crate) text: String,
}

/// 一行的文字。没有公式时返回 None，调用方照普通文字拼。
pub(crate) fn line_text(words: &[Vec<Item<'_>>], base_y: f64, base_size: f64) -> Option<String> {
    let kinds: Vec<Word> = words.iter().map(|w| classify(w)).collect();
    if !kinds.contains(&Word::Math) {
        return None;
    }
    // 公式段：连续的公式词和夹在中间的胶水词，至少有一个有数学字体作证据
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if kinds[i] != Word::Math {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > 0 && kinds[start - 1] == Word::Glue {
            start -= 1;
        }
        let mut end = i + 1;
        loop {
            let mut j = end;
            while j < words.len() && kinds[j] == Word::Glue {
                j += 1;
            }
            if j < words.len() && kinds[j] == Word::Math {
                end = j + 1;
            } else {
                // 结尾的胶水词也算（"≤ 1"），但只到下一个正文词前
                end = j;
                break;
            }
        }
        // 只有标点的首尾词留在外面
        while start < end && punct_only(&words[start]) {
            start += 1;
        }
        while end > start && punct_only(&words[end - 1]) {
            end -= 1;
        }
        if words[start..end].iter().flatten().any(evidence) {
            spans.push((start, end));
        }
        i = end.max(i + 1);
    }
    if spans.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut w = 0;
    for (start, end) in spans {
        while w < start {
            push_word(&mut out, &plain(&words[w]));
            w += 1;
        }
        let items: Vec<&Item> = words[start..end].iter().flatten().collect();
        // 末尾的句读留在公式外
        let mut cut = items.len();
        while cut > 0 && matches!(items[cut - 1].text.trim(), "," | "." | ";" | ":") {
            cut -= 1;
        }
        let tail: String = items[cut..].iter().map(|i| i.text.trim()).collect();
        // 逐词生成，词间留空格（上下标都在词内）
        let mut parts: Vec<String> = Vec::new();
        let mut taken = 0;
        for word in &words[start..end] {
            let n = word.len().min(cut.saturating_sub(taken));
            let refs: Vec<&Item> = word[..n].iter().collect();
            taken += word.len();
            let tex = tex_of(&refs, base_y, base_size);
            if !tex.is_empty() {
                parts.push(tex);
            }
        }
        let tex = parts.join(" ");
        if tex.trim().is_empty() {
            push_word(&mut out, &plain_items(&items));
        } else {
            push_word(&mut out, &format!("${}${tail}", tex.trim()));
        }
        w = end;
    }
    while w < words.len() {
        push_word(&mut out, &plain(&words[w]));
        w += 1;
    }
    Some(out)
}

fn push_word(out: &mut String, w: &str) {
    if w.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(w);
}

fn plain(word: &[Item<'_>]) -> String {
    word.iter().map(|i| i.text.trim_start()).collect()
}

fn plain_items(items: &[&Item<'_>]) -> String {
    items.iter().map(|i| i.text.trim_start()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
    Text,
    /// 数字、运算符、括号：两边是公式时算公式
    Glue,
    Math,
}

fn classify(word: &[Item<'_>]) -> Word {
    let chars: Vec<char> = word
        .iter()
        .flat_map(|i| i.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect();
    if chars.is_empty() {
        return Word::Glue;
    }
    let math_font = word
        .iter()
        .any(|i| i.g.math != MathKind::Text && !i.text.trim().is_empty());
    let symbol = chars.iter().any(|&c| is_math_symbol(c));
    let short_greek = chars.len() <= 3 && chars.iter().any(|&c| is_greek(c));
    if math_font || symbol || short_greek {
        return Word::Math;
    }
    if chars
        .iter()
        .all(|&c| c.is_ascii_digit() || "+-=<>()[]{}|/,.:;!'*−".contains(c))
    {
        return Word::Glue;
    }
    Word::Text
}

/// 公式的证据：数学字体里的字形（符号字体里的项目符号不算）。
fn evidence(item: &Item<'_>) -> bool {
    let c = item.text.trim().chars().next();
    match item.g.math {
        MathKind::Text => false,
        MathKind::Symbol => c.is_some_and(|c| {
            c.is_alphanumeric() || is_math_symbol(c) || "=+<>|()[]{}′'".contains(c)
        }),
        _ => c.is_some(),
    }
}

fn punct_only(word: &[Item<'_>]) -> bool {
    word.iter()
        .all(|i| i.text.trim().chars().all(|c| ",.;:".contains(c)))
}

fn is_greek(c: char) -> bool {
    matches!(c, 'Α'..='Ω' | 'α'..='ω' | 'ϑ' | 'ϕ' | 'ϖ' | 'ϱ' | 'ϵ' | 'ϰ')
}

fn is_math_symbol(c: char) -> bool {
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

/// 一个排版单位：一个字形（可能连着重音）对应的 TeX。
struct Atom {
    tex: String,
    x0: f64,
    x1: f64,
    y: f64,
    size: f64,
    /// 大型符号字体的字形基线不在正文基线上，不参与上下标判断
    fixed: bool,
}

const ACCENTS: &[(char, &str)] = &[
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

const FUNCS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "log", "ln", "lg", "exp", "lim", "liminf", "limsup",
    "max", "min", "sup", "inf", "det", "gcd", "arg", "deg", "dim", "ker", "hom", "Pr", "sinh",
    "cosh", "tanh", "coth", "arcsin", "arccos", "arctan",
];

fn tex_of(items: &[&Item<'_>], base_y: f64, base_size: f64) -> String {
    let mut atoms: Vec<Atom> = Vec::new();
    let mut accents: Vec<(usize, &str, f64)> = Vec::new();
    let mut k = 0;
    while k < items.len() {
        let item = items[k];
        k += 1;
        let t = item.text.trim();
        if t.is_empty() {
            continue;
        }
        let g = item.g;
        // 正文字体写的函数名：sin、log、lim……
        if g.math == MathKind::Text && t.chars().all(|c| c.is_ascii_alphabetic()) {
            let mut j = k;
            let mut name = t.to_string();
            while j < items.len()
                && items[j].g.math == MathKind::Text
                && (items[j].g.y - g.y).abs() < 0.1 * g.size
                && items[j]
                    .text
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_alphabetic())
                && !items[j].text.trim().is_empty()
            {
                name.push_str(items[j].text.trim());
                j += 1;
            }
            if FUNCS.contains(&name.as_str()) {
                atoms.push(Atom {
                    tex: format!("\\{name} "),
                    x0: g.x,
                    x1: items[j - 1].g.x_end,
                    y: g.y,
                    size: g.size,
                    fixed: false,
                });
                k = j;
                continue;
            }
        }
        let mut chars = t.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && (g.math != MathKind::Extension || is_combining(c))
            && let Some((_, cmd)) = ACCENTS.iter().find(|(a, _)| *a == c)
        {
            accents.push((atoms.len(), cmd, (g.x + g.x_end) / 2.0));
            continue;
        }
        let mut tex = String::new();
        for c in t.chars() {
            push_char(c, g.math, &mut tex);
        }
        atoms.push(Atom {
            tex,
            x0: g.x,
            x1: g.x_end.max(g.x),
            y: g.y,
            size: g.size,
            fixed: g.math == MathKind::Extension,
        });
    }
    // 重音：中心落在前一个或后一个字形上方
    for (at, cmd, cx) in accents.into_iter().rev() {
        let pick = [at, at.wrapping_sub(1)]
            .into_iter()
            .filter(|&k| k < atoms.len())
            .find(|&k| cx >= atoms[k].x0 - 0.5 && cx <= atoms[k].x1 + 0.5);
        if let Some(k) = pick {
            atoms[k].tex = format!("{cmd}{{{}}}", atoms[k].tex.trim());
        }
    }
    let mut out = String::new();
    build(&atoms, base_y, base_size, &mut out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 相对基准的层级：1 上标，-1 下标，0 同一层。
fn level(a: &Atom, base_y: f64, base_size: f64) -> i32 {
    if a.fixed || a.size > 0.85 * base_size {
        return 0;
    }
    let dy = a.y - base_y;
    if dy > 0.12 * base_size {
        1
    } else if dy < -0.08 * base_size {
        -1
    } else {
        0
    }
}

fn build(atoms: &[Atom], base_y: f64, base_size: f64, out: &mut String) {
    let mut i = 0;
    let mut have_base = false;
    // 当前底座已经有的上标、下标：再来一个同类的就另起一个空底座 {}，否则 TeX 报"重复上标"
    let (mut has_sup, mut has_sub) = (false, false);
    while i < atoms.len() {
        let lvl = level(&atoms[i], base_y, base_size);
        if lvl == 0 {
            out.push_str(&atoms[i].tex);
            have_base = true;
            has_sup = atoms[i].tex.ends_with('\'');
            has_sub = false;
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < atoms.len() && level(&atoms[j], base_y, base_size) == lvl {
            j += 1;
        }
        let group = &atoms[i..j];
        if lvl > 0 && group.iter().all(|a| a.tex.trim() == "'") && !has_sup {
            // 撇号本身就是上标
            out.push_str(&"'".repeat(group.len()));
            has_sup = true;
        } else {
            let repeated = if lvl > 0 { has_sup } else { has_sub };
            if !have_base || repeated {
                out.push_str("{}");
                has_sup = false;
                has_sub = false;
            }
            if lvl > 0 {
                has_sup = true;
            } else {
                has_sub = true;
            }
            // 组内以字号最大、最靠近基线的字形为基准再分层
            let inner = group
                .iter()
                .max_by(|a, b| {
                    a.size
                        .total_cmp(&b.size)
                        .then((b.y - base_y).abs().total_cmp(&(a.y - base_y).abs()))
                })
                .map_or((group[0].y, group[0].size), |a| (a.y, a.size));
            let mut sub = String::new();
            build(group, inner.0, inner.1, &mut sub);
            out.push_str(if lvl > 0 { "^{" } else { "_{" });
            out.push_str(sub.trim());
            out.push('}');
        }
        i = j;
    }
}

/// 字体包装：黑板体、花体、手写体、哥特体的字母。
fn wrap(kind: MathKind, c: char) -> Option<&'static str> {
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

fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x20D0..=0x20FF)
}

fn push_char(c: char, kind: MathKind, out: &mut String) {
    // 没挂到字母上的组合附加符号：TeX 里不能单独出现
    if is_combining(c) && c != '\u{0338}' {
        return;
    }
    // Adobe 私用区里的大号定界符分段（上段、中段、下段、延长段）：上段当作定界符，其余丢掉
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

/// 数学字母数字区（U+1D400–1D7FF，Cambria Math、STIX 用它写变量）：拆成字体命令和普通字母。
fn math_alnum(c: char) -> Option<(Option<&'static str>, char)> {
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
        // 数学希腊字母：各种样式都按普通希腊字母处理（斜体是默认样式）
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

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(text: &str, x: f64, y: f64, size: f64, math: MathKind) -> Glyph {
        Glyph {
            text: text.into(),
            x,
            y,
            x_end: x + 0.5 * size * text.chars().count() as f64,
            size,
            bold: false,
            math,
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

    #[test]
    fn math_alphanumerics() {
        assert_eq!(math_alnum('𝑥'), Some((None, 'x')));
        assert_eq!(math_alnum('𝔽'), Some((Some("\\mathbb"), 'F')));
        assert_eq!(math_alnum('𝟏'), Some((Some("\\mathbf"), '1')));
    }
}
