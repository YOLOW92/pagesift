//! Office Math（OMML，ECMA-376 22.1）转 LaTeX。覆盖常用结构：分数、上下标、根式、大型运算符、
//! 括号、函数、重音、上下划线、矩阵、方程组；不认识的结构只输出里面的文字，不丢内容。

use crate::Error;
use crate::xml::{Reader, Tok, local_name};

/// 一小棵 OMML 树：读完一个 `m:oMath` 再转换（公式都不大）。
#[derive(Debug, Default)]
pub(crate) struct Node {
    name: String,
    val: Option<String>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    /// 刚读到 `m:oMath`（或 `m:oMathPara`）的开始标签后调用，读到它结束。
    pub(crate) fn read(r: &mut Reader<'_>, name: &str) -> Result<Node, Error> {
        let mut stack = vec![Node {
            name: name.into(),
            ..Node::default()
        }];
        while let Some(t) = r.next()? {
            match t {
                Tok::Start(n, a) => stack.push(Node {
                    name: local_name(n).into(),
                    val: a.get("val").map(|v| v.into_owned()),
                    ..Node::default()
                }),
                Tok::End(_) => {
                    let done = stack.pop().unwrap_or_default();
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(done),
                        None => return Ok(done),
                    }
                }
                Tok::Text(t) => {
                    if let Some(top) = stack.last_mut()
                        && top.name == "t"
                    {
                        top.text.push_str(&t);
                    }
                }
            }
        }
        Ok(stack.into_iter().next().unwrap_or_default())
    }

    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// 属性子元素 `m:xxxPr/m:name@m:val`。
    fn prop(&self, pr: &str, name: &str) -> Option<&str> {
        self.child(pr)?.child(name)?.val.as_deref()
    }

    fn arg(&self, name: &str) -> String {
        self.child(name).map(|c| c.latex()).unwrap_or_default()
    }

    pub(crate) fn latex(&self) -> String {
        let b = |s: String| format!("{{{s}}}");
        match self.name.as_str() {
            "t" => self.text.chars().map(symbol).collect(),
            "f" => {
                let (n, d) = (self.arg("num"), self.arg("den"));
                match self.prop("fPr", "type") {
                    Some("lin") => format!("{n}/{d}"),
                    Some("noBar") => format!("\\genfrac{{}}{{}}{{0pt}}{{}}{}{}", b(n), b(d)),
                    _ => format!("\\frac{}{}", b(n), b(d)),
                }
            }
            "sSup" => format!("{}^{}", b(self.arg("e")), b(self.arg("sup"))),
            "sSub" => format!("{}_{}", b(self.arg("e")), b(self.arg("sub"))),
            "sSubSup" => format!(
                "{}_{}^{}",
                b(self.arg("e")),
                b(self.arg("sub")),
                b(self.arg("sup"))
            ),
            "sPre" => format!(
                "{{}}_{}^{}{}",
                b(self.arg("sub")),
                b(self.arg("sup")),
                b(self.arg("e"))
            ),
            "rad" => {
                let deg = self.arg("deg");
                if deg.trim().is_empty() {
                    format!("\\sqrt{}", b(self.arg("e")))
                } else {
                    format!("\\sqrt[{deg}]{}", b(self.arg("e")))
                }
            }
            "nary" => {
                let op = match self.prop("naryPr", "chr").unwrap_or("∫") {
                    "∑" => "\\sum",
                    "∏" => "\\prod",
                    "∐" => "\\coprod",
                    "∬" => "\\iint",
                    "∭" => "\\iiint",
                    "∮" => "\\oint",
                    "⋃" => "\\bigcup",
                    "⋂" => "\\bigcap",
                    _ => "\\int",
                };
                let (sub, sup) = (self.arg("sub"), self.arg("sup"));
                let mut s = op.to_string();
                if !sub.is_empty() {
                    s += &format!("_{}", b(sub));
                }
                if !sup.is_empty() {
                    s += &format!("^{}", b(sup));
                }
                format!("{s} {}", self.arg("e"))
            }
            "d" => {
                let open = self.prop("dPr", "begChr").unwrap_or("(");
                let close = self.prop("dPr", "endChr").unwrap_or(")");
                let sep = self.prop("dPr", "sepChr").unwrap_or("|");
                let inner: Vec<String> = self
                    .children
                    .iter()
                    .filter(|c| c.name == "e")
                    .map(Node::latex)
                    .collect();
                format!(
                    "\\left{} {} \\right{}",
                    delim(open),
                    inner.join(sep),
                    delim(close)
                )
            }
            "func" => format!("{} {}", self.arg("fName"), self.arg("e")),
            "acc" => {
                let cmd = match self.prop("accPr", "chr").unwrap_or("\u{302}") {
                    "\u{307}" => "\\dot",
                    "\u{308}" => "\\ddot",
                    "\u{303}" => "\\tilde",
                    "\u{304}" | "\u{305}" => "\\bar",
                    "\u{20D7}" | "\u{2192}" => "\\vec",
                    _ => "\\hat",
                };
                format!("{cmd}{}", b(self.arg("e")))
            }
            "bar" => {
                let cmd = if self.prop("barPr", "pos") == Some("bot") {
                    "\\underline"
                } else {
                    "\\overline"
                };
                format!("{cmd}{}", b(self.arg("e")))
            }
            "limLow" => format!("{}_{}", b(self.arg("e")), b(self.arg("lim"))),
            "limUpp" => format!("{}^{}", b(self.arg("e")), b(self.arg("lim"))),
            "groupChr" => format!("\\underbrace{}", b(self.arg("e"))),
            "m" => {
                let rows: Vec<String> = self
                    .children
                    .iter()
                    .filter(|c| c.name == "mr")
                    .map(|r| {
                        r.children
                            .iter()
                            .filter(|c| c.name == "e")
                            .map(Node::latex)
                            .collect::<Vec<_>>()
                            .join(" & ")
                    })
                    .collect();
                format!("\\begin{{matrix}} {} \\end{{matrix}}", rows.join(" \\\\ "))
            }
            "eqArr" => {
                let rows: Vec<String> = self
                    .children
                    .iter()
                    .filter(|c| c.name == "e")
                    .map(Node::latex)
                    .collect();
                format!(
                    "\\begin{{aligned}} {} \\end{{aligned}}",
                    rows.join(" \\\\ ")
                )
            }
            // 属性元素不输出
            n if n.ends_with("Pr") => String::new(),
            _ => self.children.iter().map(Node::latex).collect(),
        }
    }
}

fn delim(c: &str) -> &str {
    match c {
        "" => ".",
        "{" => "\\{",
        "}" => "\\}",
        "⟨" | "〈" => "\\langle",
        "⟩" | "〉" => "\\rangle",
        "‖" => "\\|",
        "⌈" => "\\lceil",
        "⌉" => "\\rceil",
        "⌊" => "\\lfloor",
        "⌋" => "\\rfloor",
        c => c,
    }
}

/// 公式文字里需要转义的 LaTeX 特殊字符；其余（希腊字母等 Unicode）KaTeX/MathJax 都能直接显示。
fn symbol(c: char) -> String {
    match c {
        '{' | '}' | '%' | '#' | '&' | '$' | '_' => format!("\\{c}"),
        '\\' => "\\backslash ".into(),
        '−' => "-".into(),
        '×' => "\\times ".into(),
        '÷' => "\\div ".into(),
        '≤' => "\\le ".into(),
        '≥' => "\\ge ".into(),
        '≠' => "\\ne ".into(),
        '±' => "\\pm ".into(),
        '∞' => "\\infty ".into(),
        '→' => "\\to ".into(),
        '∈' => "\\in ".into(),
        '⋅' | '·' => "\\cdot ".into(),
        c => c.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(xml: &str) -> String {
        let mut r = Reader::new(xml);
        let Some(Tok::Start(n, _)) = r.next().unwrap() else {
            panic!()
        };
        Node::read(&mut r, local_name(n)).unwrap().latex()
    }

    #[test]
    fn converts_common_structures() {
        let frac = r#"<m:oMath><m:f><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b+1</m:t></m:r></m:den></m:f></m:oMath>"#;
        assert_eq!(conv(frac), "\\frac{a}{b+1}");
        let sup = r#"<m:oMath><m:sSup><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath>"#;
        assert_eq!(conv(sup), "{x}^{2}");
        let sum = r#"<m:oMath><m:nary><m:naryPr><m:chr m:val="∑"/></m:naryPr><m:sub><m:r><m:t>i=1</m:t></m:r></m:sub><m:sup><m:r><m:t>n</m:t></m:r></m:sup><m:e><m:r><m:t>i</m:t></m:r></m:e></m:nary></m:oMath>"#;
        assert_eq!(conv(sum), "\\sum_{i=1}^{n} i");
        let rad = r#"<m:oMath><m:rad><m:radPr><m:degHide m:val="1"/></m:radPr><m:deg/><m:e><m:r><m:t>2</m:t></m:r></m:e></m:rad></m:oMath>"#;
        assert_eq!(conv(rad), "\\sqrt{2}");
        let paren = r#"<m:oMath><m:d><m:dPr><m:begChr m:val="["/><m:endChr m:val="]"/></m:dPr><m:e><m:r><m:t>x≤y</m:t></m:r></m:e></m:d></m:oMath>"#;
        assert_eq!(conv(paren), "\\left[ x\\le y \\right]");
    }
}
