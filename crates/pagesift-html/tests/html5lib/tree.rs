//! 两边都转成同一种树再比较：预期结果从 `#document` 的缩进文本读出来，
//! pagesift 的结果从 [`Html`] 转过来。

use pagesift_html::{Html, Namespace, NodeRef};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 命名空间前缀（""、"svg"、"math"）、名字、按名字排好序的属性。
    Element {
        ns: String,
        name: String,
        attrs: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
    /// DOM 里只记有没有 DOCTYPE，名字和 id 不比较。
    Doctype,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Node {
    pub kind: Kind,
    pub children: Vec<Node>,
}

// ---------------------------------------------------------------- 预期结果

/// 读 `#document` 段。`<template>` 的 `content` 层去掉，内容直接作为 template 的子节点
/// （pagesift 不建独立的文档片段，见 builder 模块说明）。
pub fn parse_expected(dump: &str) -> Vec<Node> {
    // 先把多行文本、属性值拼回一行一个条目
    let mut items: Vec<(usize, String)> = Vec::new();
    for line in dump.trim_end_matches('\n').split('\n') {
        if let Some(rest) = line.strip_prefix("| ") {
            let depth = rest.len() - rest.trim_start_matches(' ').len();
            items.push((depth / 2, rest[depth..].to_string()));
        } else if let Some(last) = items.last_mut() {
            last.1.push('\n');
            last.1.push_str(line);
        }
    }
    let mut root: Vec<Node> = Vec::new();
    // 打开的元素在树里的路径（每层是子节点下标），和它们的深度
    let mut path: Vec<(usize, usize)> = Vec::new();
    // template 的 content 层：深度 d 的 content 让更深的节点整体上移一层
    let mut shifts: Vec<usize> = Vec::new();
    for (depth, item) in items {
        while shifts.last().is_some_and(|&d| d >= depth) {
            shifts.pop();
        }
        if item == "content"
            && path
                .last()
                .is_some_and(|&(d, _)| d + 1 == depth && is_template(&root, &path))
        {
            shifts.push(depth);
            continue;
        }
        let depth = depth - shifts.len();
        while path.last().is_some_and(|&(d, _)| d >= depth) {
            path.pop();
        }
        // 属性：挂在当前元素上
        // 属性行是 `名字="值"`；名字本身可能以 `<` 开头（`<img <="">`），元素行以 `>` 结尾
        let element = item.starts_with('<') && item.ends_with('>');
        if !element && !item.starts_with('"') {
            if let Some((k, v)) = item.split_once("=\"") {
                let v = v.strip_suffix('"').unwrap_or(v);
                if let Some(Kind::Element { attrs, .. }) =
                    node_at(&mut root, &path).map(|n| &mut n.kind)
                {
                    attrs.push((k.to_string(), v.to_string()));
                }
            }
            continue;
        }
        let kind = if let Some(t) = item.strip_prefix('"') {
            Kind::Text(t.strip_suffix('"').unwrap_or(t).to_string())
        } else if let Some(c) = item.strip_prefix("<!-- ") {
            Kind::Comment(c.strip_suffix(" -->").unwrap_or(c).to_string())
        } else if item.starts_with("<!DOCTYPE") {
            Kind::Doctype
        } else if item.starts_with("<?") {
            // 处理指令：HTML 解析器按伪注释处理，这里同样当注释
            Kind::Comment(item.clone())
        } else {
            let inner = item.trim_start_matches('<').trim_end_matches('>');
            let (ns, name) = match inner.split_once(' ') {
                Some((ns @ ("svg" | "math"), name)) => (ns.to_string(), name.to_string()),
                _ => (String::new(), inner.to_string()),
            };
            Kind::Element {
                ns,
                name,
                attrs: Vec::new(),
            }
        };
        let siblings = match node_at(&mut root, &path) {
            Some(n) => &mut n.children,
            None => &mut root,
        };
        siblings.push(Node {
            kind,
            children: Vec::new(),
        });
        let idx = siblings.len() - 1;
        path.push((depth, idx));
    }
    sort_attrs(&mut root);
    root
}

fn node_at<'n>(root: &'n mut [Node], path: &[(usize, usize)]) -> Option<&'n mut Node> {
    let (&(_, first), rest) = path.split_first()?;
    let mut n = &mut root[first];
    for &(_, i) in rest {
        n = &mut n.children[i];
    }
    Some(n)
}

fn is_template(root: &[Node], path: &[(usize, usize)]) -> bool {
    let Some((&(_, first), rest)) = path.split_first() else {
        return false;
    };
    let mut n = &root[first];
    for &(_, i) in rest {
        n = &n.children[i];
    }
    matches!(&n.kind, Kind::Element { ns, name, .. } if ns.is_empty() && name == "template")
}

fn sort_attrs(nodes: &mut [Node]) {
    for n in nodes {
        if let Kind::Element { attrs, .. } = &mut n.kind {
            attrs.sort();
        }
        sort_attrs(&mut n.children);
    }
}

// ---------------------------------------------------------------- pagesift 的结果

/// 命名空间取 DOM 里记录的（解析器按规范定下的）。SVG 元素名、属性名的大小写和
/// xlink/xml/xmlns 前缀按规范的对照表修正（解析器存小写名）。
pub fn from_html(doc: &Html<'_>) -> Vec<Node> {
    doc.root().children().map(convert).collect()
}

/// 片段解析的结果：上下文元素的子节点（[`Html::parse_fragment`] 里根元素的子节点）。
pub fn from_fragment(doc: &Html<'_>) -> Vec<Node> {
    doc.root()
        .children()
        .filter(|c| c.is_element())
        .flat_map(|html| html.children())
        .map(convert)
        .collect()
}

fn convert(n: NodeRef<'_>) -> Node {
    let kind = if let Some(el) = n.as_element() {
        let lname = el.name();
        let ns = match el.namespace() {
            Namespace::Html => "",
            Namespace::Svg => "svg",
            Namespace::MathMl => "math",
        };
        let name = if ns == "svg" {
            svg_tag_case(lname).unwrap_or(lname).to_string()
        } else {
            lname.to_string()
        };
        let mut attrs: Vec<(String, String)> = el
            .attrs()
            .map(|(k, v)| {
                let k = if ns.is_empty() {
                    k.to_string()
                } else {
                    foreign_attr(ns, k)
                };
                (k, v.to_string())
            })
            .collect();
        attrs.sort();
        let children = n.children().map(convert).collect();
        return Node {
            kind: Kind::Element {
                ns: ns.to_string(),
                name,
                attrs,
            },
            children,
        };
    } else if let Some(t) = n.as_text() {
        Kind::Text(t.to_string())
    } else if let Some(c) = n.as_comment() {
        Kind::Comment(c.to_string())
    } else {
        Kind::Doctype
    };
    Node {
        kind,
        children: Vec::new(),
    }
}

/// 规范里 SVG 元素名的大小写修正表（解析器存的是小写名，修正只取决于名字本身）。
fn svg_tag_case(name: &str) -> Option<&'static str> {
    const TABLE: &[&str] = &[
        "altGlyph",
        "altGlyphDef",
        "altGlyphItem",
        "animateColor",
        "animateMotion",
        "animateTransform",
        "clipPath",
        "feBlend",
        "feColorMatrix",
        "feComponentTransfer",
        "feComposite",
        "feConvolveMatrix",
        "feDiffuseLighting",
        "feDisplacementMap",
        "feDistantLight",
        "feDropShadow",
        "feFlood",
        "feFuncA",
        "feFuncB",
        "feFuncG",
        "feFuncR",
        "feGaussianBlur",
        "feImage",
        "feMerge",
        "feMergeNode",
        "feMorphology",
        "feOffset",
        "fePointLight",
        "feSpecularLighting",
        "feSpotLight",
        "feTile",
        "feTurbulence",
        "foreignObject",
        "glyphRef",
        "linearGradient",
        "radialGradient",
        "textPath",
    ];
    TABLE.iter().copied().find(|t| t.eq_ignore_ascii_case(name))
}

/// 外部元素属性的修正：SVG 属性大小写、MathML 的 definitionURL、xlink/xml/xmlns 命名空间。
fn foreign_attr(ns: &str, name: &str) -> String {
    const SVG: &[&str] = &[
        "attributeName",
        "attributeType",
        "baseFrequency",
        "baseProfile",
        "calcMode",
        "clipPathUnits",
        "diffuseConstant",
        "edgeMode",
        "filterUnits",
        "glyphRef",
        "gradientTransform",
        "gradientUnits",
        "kernelMatrix",
        "kernelUnitLength",
        "keyPoints",
        "keySplines",
        "keyTimes",
        "lengthAdjust",
        "limitingConeAngle",
        "markerHeight",
        "markerUnits",
        "markerWidth",
        "maskContentUnits",
        "maskUnits",
        "numOctaves",
        "pathLength",
        "patternContentUnits",
        "patternTransform",
        "patternUnits",
        "pointsAtX",
        "pointsAtY",
        "pointsAtZ",
        "preserveAlpha",
        "preserveAspectRatio",
        "primitiveUnits",
        "refX",
        "refY",
        "repeatCount",
        "repeatDur",
        "requiredExtensions",
        "requiredFeatures",
        "specularConstant",
        "specularExponent",
        "spreadMethod",
        "startOffset",
        "stdDeviation",
        "stitchTiles",
        "surfaceScale",
        "systemLanguage",
        "tableValues",
        "targetX",
        "targetY",
        "textLength",
        "viewBox",
        "viewTarget",
        "xChannelSelector",
        "yChannelSelector",
        "zoomAndPan",
    ];
    match name {
        "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show"
        | "xlink:title" | "xlink:type" | "xml:lang" | "xml:space" | "xmlns:xlink" => {
            name.replacen(':', " ", 1)
        }
        "xmlns" => "xmlns xmlns".to_string(),
        "definitionurl" if ns == "math" => "definitionURL".to_string(),
        _ if ns == "svg" => SVG
            .iter()
            .find(|a| a.eq_ignore_ascii_case(name))
            .map_or_else(|| name.to_string(), |a| a.to_string()),
        _ => name.to_string(),
    }
}

// ---------------------------------------------------------------- 输出

pub fn dump(nodes: &[Node]) -> String {
    let mut out = String::new();
    for n in nodes {
        dump_node(n, 0, &mut out);
    }
    out
}

fn dump_node(n: &Node, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match &n.kind {
        Kind::Element { ns, name, attrs } => {
            if ns.is_empty() {
                out.push_str(&format!("| {pad}<{name}>\n"));
            } else {
                out.push_str(&format!("| {pad}<{ns} {name}>\n"));
            }
            for (k, v) in attrs {
                out.push_str(&format!("| {pad}  {k}=\"{v}\"\n"));
            }
        }
        Kind::Text(t) => out.push_str(&format!("| {pad}\"{t}\"\n")),
        Kind::Comment(c) => out.push_str(&format!("| {pad}<!-- {c} -->\n")),
        Kind::Doctype => out.push_str(&format!("| {pad}<!DOCTYPE>\n")),
    }
    for c in &n.children {
        dump_node(c, depth + 1, out);
    }
}
