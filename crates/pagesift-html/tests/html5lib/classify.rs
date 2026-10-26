//! 差异分类：
//! - **影响正文**：文本内容、顺序或块结构不同。比较的是“正文投影”：按文档顺序排列的块级
//!   元素边界（含 html/head/body，文字落在 head 还是 body 也算）和其间的文字（空白折叠）；
//!   行内元素、属性、注释、DOCTYPE、命名空间都不看。正文流水线整棵跳过、永远不产出正文的
//!   元素（pagesift-content 的 `is_non_content_tag`：script、style、template、select、svg 等）
//!   只留边界，里面的结构不看；文字被错放进这类元素时，投影里那段文字会消失，仍然算影响正文。
//! - **只差行内结构**：投影相同，但行内元素的嵌套不同（比如 adoption agency 的细节让
//!   一段文字加粗或者没加粗、链接多包了一层）。
//! - **只差不产出正文的元素内部**：差异全在 script、template、select 这类元素里面。
//! - **其他**：只差属性、注释位置、DOCTYPE、命名空间、纯空白文本节点的位置。

use super::tree::{Kind, Node};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    Pass,
    Content,
    Inline,
    Inert,
    Minor,
}

impl Class {
    pub fn label(self) -> &'static str {
        match self {
            Class::Pass => "通过",
            Class::Content => "影响正文",
            Class::Inline => "只差行内结构",
            Class::Inert => "只差不产出正文的元素内部",
            Class::Minor => "其他（不影响正文）",
        }
    }
}

pub fn classify(expected: &[Node], actual: &[Node]) -> Class {
    if expected == actual {
        return Class::Pass;
    }
    if projection(expected) != projection(actual) {
        return Class::Content;
    }
    if shape(expected, true) != shape(actual, true) {
        return Class::Inline;
    }
    if shape(expected, false) != shape(actual, false) {
        return Class::Inert;
    }
    Class::Minor
}

/// 块级（以及装文字的容器类）HTML 元素：它们的边界决定文字分在哪一段、哪个单元格。
fn block(name: &str) -> bool {
    matches!(
        name,
        "html"
            | "head"
            | "body"
            | "address"
            | "article"
            | "aside"
            | "blockquote"
            | "caption"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "listing"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "optgroup"
            | "option"
            | "p"
            | "plaintext"
            | "pre"
            | "search"
            | "section"
            | "select"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "textarea"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
            | "xmp"
            | "title"
            | "script"
            | "style"
            | "noscript"
            | "noframes"
            | "iframe"
            | "noembed"
            | "template"
            | "object"
            | "button"
            | "colgroup"
    )
}

/// pagesift-content `dom::is_non_content_tag` 里的元素（head、title 除外：标题进元数据）。
fn skipped(ns: &str, name: &str) -> bool {
    match ns {
        "" => matches!(
            name,
            "script"
                | "style"
                | "noscript"
                | "template"
                | "iframe"
                | "canvas"
                | "object"
                | "embed"
                | "select"
                | "option"
                | "input"
                | "textarea"
                | "button"
                | "dialog"
        ),
        "svg" => name == "svg",
        _ => false,
    }
}

pub fn projection(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    let mut text = String::new();
    for n in nodes {
        project(n, &mut out, &mut text);
    }
    flush(&mut out, &mut text);
    out
}

fn project(n: &Node, out: &mut Vec<String>, text: &mut String) {
    match &n.kind {
        Kind::Text(t) => text.push_str(t),
        Kind::Element { ns, name, .. } if skipped(ns, name) => {
            flush(out, text);
            out.push(format!("<{name}/>"));
        }
        Kind::Element { ns, name, .. } if ns.is_empty() && block(name) => {
            flush(out, text);
            out.push(format!("<{name}>"));
            for c in &n.children {
                project(c, out, text);
            }
            flush(out, text);
            out.push(format!("</{name}>"));
        }
        Kind::Element { ns, name, .. } => {
            // br 在输出里是换行，和空白等价
            if ns.is_empty() && name == "br" {
                text.push(' ');
            }
            for c in &n.children {
                project(c, out, text);
            }
        }
        Kind::Comment(_) | Kind::Doctype => {}
    }
}

fn flush(out: &mut Vec<String>, text: &mut String) {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !collapsed.is_empty() {
        out.push(collapsed);
    }
    text.clear();
}

/// 只留元素（名字不分大小写、不带属性）和非空白文字的树，用来判断“只差行内结构”。
/// `skip_inert` 时不产出正文的元素只留边界。
fn shape(nodes: &[Node], skip_inert: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut text = String::new();
    for n in nodes {
        shape_node(n, skip_inert, &mut out, &mut text);
    }
    flush(&mut out, &mut text);
    out
}

fn shape_node(n: &Node, skip_inert: bool, out: &mut Vec<String>, text: &mut String) {
    match &n.kind {
        Kind::Text(t) => text.push_str(t),
        Kind::Element { ns, name, .. } if skip_inert && skipped(ns, name) => {
            flush(out, text);
            out.push(format!("<{ns}:{name}/>"));
        }
        Kind::Element { ns, name, .. } => {
            flush(out, text);
            out.push(format!("<{ns}:{}>", name.to_ascii_lowercase()));
            for c in &n.children {
                shape_node(c, skip_inert, out, text);
            }
            flush(out, text);
            out.push("</>".to_string());
        }
        Kind::Comment(_) | Kind::Doctype => {}
    }
}
