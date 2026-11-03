//! 解码文本 → LaTeX 的后处理，照 `PPFormulaNetProcessor.post_process_generation`：
//! 去掉包着中文的 `\text{…}` 和所有双引号，再按 `normalize` 删去多余空格。
//!
//! 参考实现用正则；这里逐条按 Python `re` 的匹配语义（自左向右、不重叠、惰性量词取最短）手写，
//! 不引入正则库。参考实现在两步之间还调 ftfy 的 `fix_text`（没装 ftfy 时跳过），这里不做：
//! 它只修乱码、全角、弯引号这类 Unicode 问题，分词器的 NFKC 已把训练文本里的这些归一化掉了。

/// [协议] `normalize` 给 `\text{…}` 里的宏打的临时记号，最后换回一个空格
const MARK: &str = "XXXXXXX";

/// 参考实现 `_text_reg` 里的四个命令，也是 `_protected_macros`
const TEXT_COMMANDS: &[&str] = &["operatorname", "mathrm", "text", "mathbf"];

/// 把解码出的文本整理成 LaTeX。
pub fn to_latex(text: &str) -> String {
    render_safe(&normalize(&remove_chinese_text_wrapping(text)))
}

/// 参考实现之外的一步：模型会输出 `\textlangle`、`\textrangle`，KaTeX 不认，整个公式渲染失败；
/// 换成数学模式下同一个符号。
fn render_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("\\text") {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        let word = tail[1..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .map_or(tail.len(), |n| n + 1);
        out.push_str(match &tail[..word] {
            "\\textlangle" => "\\langle",
            "\\textrangle" => "\\rangle",
            other => other,
        });
        rest = &tail[word..];
    }
    out.push_str(rest);
    out
}

/// Python `re` 的 `\s`（Unicode 空白；str 模式下还含 U+001C–U+001F）。
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python `re` 的 `\w`（str 模式：`str.isalnum()` 或下划线）。
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 参考实现的 `letter = [a-zA-Z]`。
fn is_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// 参考实现的 `noletter = [\W_^\d]`。`\d` 是 Unicode 的 Nd 类，这里用 `is_numeric`（含 Nl、No）
/// 近似：只在 `²`、`½` 这类字符上有别，NFKC 归一化过的输出里不会有。
fn is_noletter(c: char) -> bool {
    !is_word(c) || c == '_' || c == '^' || c.is_numeric()
}

fn starts_with(c: &[char], i: usize, s: &str) -> bool {
    (i..).zip(s.chars()).all(|(k, x)| c.get(k) == Some(&x))
}

/// `re.sub(r"\\text\s*{([^{}]*[\u4e00-\u9fff]+[^{}]*)}", r"\1", s)` 后去掉所有 `"`。
fn remove_chinese_text_wrapping(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if let Some((inner, end)) = chinese_text_at(&c, i) {
            out.extend(&c[inner..end - 1]);
            i = end;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out.replace('"', "")
}

/// 从 `i` 起是否匹配 `\text\s*{…}`，花括号里没有花括号且至少有一个 CJK 统一汉字；
/// 是则返回（花括号内起点，匹配终点）。
fn chinese_text_at(c: &[char], i: usize) -> Option<(usize, usize)> {
    if !starts_with(c, i, "\\text") {
        return None;
    }
    let mut j = i + "\\text".len();
    while c.get(j).is_some_and(|&x| is_space(x)) {
        j += 1;
    }
    if c.get(j) != Some(&'{') {
        return None;
    }
    let start = j + 1;
    let close = start + c[start..].iter().position(|&x| x == '{' || x == '}')?;
    // [协议] 参考正则里的 CJK 统一汉字基本区 U+4E00–U+9FFF
    let cjk = |x: &char| ('\u{4e00}'..='\u{9fff}').contains(x);
    (c[close] == '}' && c[start..close].iter().any(cjk)).then_some((start, close + 1))
}

/// 参考实现的 `normalize`：
/// 1. 对原文里每处 `\(operatorname|mathrm|text|mathbf)\s?\*? {.*?}`，找出其中后接空白再接
///    单词字符的宏（如 `\alpha b` 的 `\alpha`），不是这四个命令本身的，就在全文把它换成
///    “宏 + 记号”并删去全文所有空格（参考实现随后用 `_text_reg.sub` 回填，但空格删光后
///    `_text_reg` 必须的那个空格已不存在，匹配不到，是空操作，这里省掉）；
/// 2. 反复删去 非字母–非字母、非字母–字母、字母–非字母 之间的空白，直到不再变化；
/// 3. 记号换回一个空格。
fn normalize(text: &str) -> String {
    let mut s = text.to_owned();
    for group in text_groups(text) {
        for m in protected_candidates(&group) {
            if !TEXT_COMMANDS.contains(&&m[1..]) {
                s = s.replace(&m, &format!("{m}{MARK}"));
                s = s.replace(' ', "");
            }
        }
    }
    let mut cur: Vec<char> = s.chars().collect();
    loop {
        let next = squeeze(&cur, is_noletter, is_noletter, true);
        let next = squeeze(&next, is_noletter, is_letter, true);
        let next = squeeze(&next, is_letter, is_noletter, false);
        if next == cur {
            break;
        }
        cur = next;
    }
    cur.into_iter().collect::<String>().replace(MARK, " ")
}

/// `_text_reg.findall` 的各处整段匹配（`x[0]`）。
fn text_groups(text: &str) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match text_group_at(&c, i) {
            Some(end) => {
                out.push(c[i..end].iter().collect());
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// 从 `i` 起匹配 `\\(operatorname|mathrm|text|mathbf)\s?\*? {.*?}`，返回终点。
/// 回溯顺序同 `re`：`\s?`、`\*?` 都先试取再试不取；`.*?` 取到第一个 `}`，不跨换行。
fn text_group_at(c: &[char], i: usize) -> Option<usize> {
    if c.get(i) != Some(&'\\') {
        return None;
    }
    for cmd in TEXT_COMMANDS {
        if !starts_with(c, i + 1, cmd) {
            continue;
        }
        let after = i + 1 + cmd.len();
        for take_space in [true, false] {
            if take_space && !c.get(after).is_some_and(|&x| is_space(x)) {
                continue;
            }
            let p = after + usize::from(take_space);
            for take_star in [true, false] {
                if take_star && c.get(p) != Some(&'*') {
                    continue;
                }
                let q = p + usize::from(take_star);
                if c.get(q) != Some(&' ') || c.get(q + 1) != Some(&'{') {
                    continue;
                }
                let body = q + 2;
                if let Some(k) = c[body..].iter().position(|&x| x == '}' || x == '\n')
                    && c[body + k] == '}'
                {
                    return Some(body + k + 1);
                }
            }
        }
    }
    None
}

/// `_macro_pattern.findall`：`(\\[a-zA-Z]+)\s(?=\w)|\\[a-zA-Z]+\s(?=})`。第一支给出宏名，
/// 第二支的分组为空（参考实现随后按空串滤掉），这里只返回第一支的宏名。
fn protected_candidates(group: &str) -> Vec<String> {
    let c: Vec<char> = group.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\\' {
            let mut j = i + 1;
            while c.get(j).is_some_and(|&x| is_letter(x)) {
                j += 1;
            }
            // 字母串只有取满时后面才可能是空白
            if j > i + 1 && c.get(j).is_some_and(|&x| is_space(x)) {
                match c.get(j + 1) {
                    Some(&x) if is_word(x) => {
                        out.push(c[i..j].iter().collect());
                        i = j + 1;
                        continue;
                    }
                    Some('}') => {
                        i = j + 1;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    out
}

/// `re.sub(r"(?!\\ )(A)\s+?(B)", r"\1\2", s)`（`guard` 为假时没有开头的否定前瞻）：
/// 自左向右，A 后面取最少的空白使紧接着的字符属于 B，删去这段空白；匹配后从 B 之后继续。
fn squeeze(
    c: &[char],
    first: impl Fn(char) -> bool,
    second: impl Fn(char) -> bool,
    guard: bool,
) -> Vec<char> {
    let mut out = Vec::with_capacity(c.len());
    let mut i = 0;
    while i < c.len() {
        let blocked = guard && c[i] == '\\' && c.get(i + 1) == Some(&' ');
        if !blocked && first(c[i]) {
            let mut k = 1;
            let mut hit = None;
            while c.get(i + k).is_some_and(|&x| is_space(x)) {
                if c.get(i + k + 1).is_some_and(|&x| second(x)) {
                    hit = Some(k);
                    break;
                }
                k += 1;
            }
            if let Some(k) = hit {
                out.push(c[i]);
                out.push(c[i + k + 1]);
                i += k + 2;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 期望值是把参考实现的 `post_process_generation`（去掉 ftfy）用 Python 标准库 `re`
    /// 原样跑出来的；第一条的结果与 transformers 文档里该模型的样例输出逐字相同。
    #[test]
    fn replaces_symbols_katex_rejects() {
        assert_eq!(
            render_safe(r"\textlangle x \textrangle + \textbf{a} \textlangles"),
            r"\langle x \rangle + \textbf{a} \textlangles"
        );
    }

    #[test]
    fn matches_reference_post_process() {
        let cases = [
            (
                r"\zeta _ { 0 } ( \nu ) = - \frac { \nu \varrho ^ { - 2 \nu } } { \pi } \int _ { \mu } ^ { \infty } d \omega \int _ { C _ { + } } d z \frac { 2 z ^ { 2 } } { ( z ^ { 2 } + \omega ^ { 2 } ) ^ { \nu + 1 } } \breve { \Psi } ( \omega ; z ) e ^ { i \epsilon z } \quad ,",
                r"\zeta_{0}(\nu)=-\frac{\nu\varrho^{-2\nu}}{\pi}\int_{\mu}^{\infty}d\omega\int_{C_{+}}d z\frac{2z^{2}}{(z^{2}+\omega^{2})^{\nu+1}}\breve{\Psi}(\omega;z)e^{i\epsilon z}\quad,",
            ),
            (
                r"\mathrm { d } x + \mathrm {d} y",
                r"\mathrm{d}x+\mathrm{d}y",
            ),
            (r"\mathrm { \alpha b } + c", r"\mathrm{\alpha b}+c"),
            (
                r"\text { if \quad x } , \operatorname* { a r g }",
                r"\text{if\quad x},\operatorname*{arg}",
            ),
            (r"a \ b + \ c", r"a\ b+\ c"),
            ("x  =   y", "x=y"),
            ("\\text{中文 abc} + \\text {\"q\"}", r"中文 abc+\text{q}"),
            ("a\n+ b", "a+b"),
            (
                r"\mathbf { \alpha x } \mathbf { \alpha y }",
                r"\mathbf{\alpha  x}\mathbf{\alpha  y}",
            ),
            ("1 2 3 a b c", "123a b c"),
            (r"_ ^ { } \, \;", r"_^{}\,\;"),
            (r"\left( a \right)", r"\left(a\right)"),
            (
                r"\mathrm { \alpha \beta } \text { \gamma }",
                r"\mathrm{\alpha\beta}\text{\gamma}",
            ),
            ("\\text  { a } \\text {\nb }", r"\text{a}\text{b}"),
        ];
        for (input, want) in cases {
            assert_eq!(to_latex(input), want, "输入 {input:?}");
        }
    }
}
