//! DOM 子树转 GFM。分两层：块级渲染产出段落列表，行内渲染产出单行文本。

use pagesift_html::{Element, Tag};
use url::Url;

use crate::dom::{NodeR, collapse_whitespace, collapse_whitespace_into, element, raw_text, tag};
use crate::extract::Skipper;
use crate::math::{self, Math};

mod block;
mod escape;
mod inline;

use escape::*;
use inline::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Para,
    List,
    Other,
}

struct Block {
    kind: Kind,
    text: String,
}

pub(crate) struct Renderer<'a> {
    pub base: Option<&'a Url>,
    pub skipper: &'a Skipper,
    pub whitelist: Whitelist,
    /// 扁平模式：处于表格单元格或排版表内部。此时 `table` 不再展开成子表，
    /// 内层表格只按普通容器把文字透传上来，避免嵌套表格被逐层重复渲染。
    flat: bool,
}

/// 只输出哪些元素里的文字（照 trafilatura 的 TAG_CATALOG）。文章模式里，散落在 `div`、`span`、
/// `section` 里的行内文字（署名、日期、图注、按钮、"阅读更多"）不是正文段落。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Whitelist {
    /// 所有容器的文字都输出（默认）。
    Off,
    /// 只输出段落、标题、列表项、引用、代码、表格单元格里的文字；`div` 为真时
    /// 只装文字的 `div` 也算段落（页面不用 `<p>` 排版时）。
    On { div: bool },
}

impl<'a> Renderer<'a> {
    pub(crate) fn new(base: Option<&'a Url>, skipper: &'a Skipper) -> Self {
        Self {
            base,
            skipper,
            whitelist: Whitelist::Off,
            flat: false,
        }
    }

    /// 进入扁平模式：内层表格不再渲染成子表，只透传文字。
    fn as_flat(&self) -> Self {
        Self {
            base: self.base,
            skipper: self.skipper,
            whitelist: self.whitelist,
            flat: true,
        }
    }

    pub(crate) fn whitelist(mut self, w: Whitelist) -> Self {
        self.whitelist = w;
        self
    }

    /// `node` 里直接写的行内文字能不能成为一个段落。
    fn paragraph_allowed(&self, node: &NodeR<'_>) -> bool {
        match self.whitelist {
            Whitelist::Off => true,
            Whitelist::On { .. } if self.skipper.editorial_attachment(node) => true,
            Whitelist::On { div } => match node.tag() {
                Some(
                    Tag::P
                    | Tag::Blockquote
                    | Tag::Li
                    | Tag::Dd
                    | Tag::Dt
                    | Tag::Td
                    | Tag::Th
                    | Tag::Pre
                    | Tag::Code,
                ) => true,
                Some(Tag::Div) => div || crate::dom::is_heading(node),
                _ => false,
            },
        }
    }
}

pub(crate) fn is_block_tag(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        Address
            | Article
            | Aside
            | Blockquote
            | Center
            | Details
            | Dd
            | Dialog
            | Div
            | Dl
            | Dt
            | Fieldset
            | Figcaption
            | Figure
            | Footer
            | Form
            | H1
            | H2
            | H3
            | H4
            | H5
            | H6
            | Header
            | Hgroup
            | Hr
            | Li
            | Main
            | Nav
            | Ol
            | P
            | Pre
            | Section
            | Summary
            | Table
            | Tbody
            | Td
            | Tfoot
            | Th
            | Thead
            | Tr
            | Ul
            | Body
            | Html
    )
}

fn is_inline_tag(tag: Tag) -> bool {
    use Tag::*;
    matches!(
        tag,
        A | Abbr
            | B
            | Bdi
            | Bdo
            | Br
            | Cite
            | Code
            | Data
            | Del
            | Dfn
            | Em
            | I
            | Img
            | Ins
            | Kbd
            | Label
            | Mark
            | Q
            | S
            | Samp
            | Small
            | Span
            | Strike
            | Strong
            | Sub
            | Sup
            | Time
            | Tt
            | U
            | Var
            | Wbr
            | Font
            | Picture
    )
}

impl Renderer<'_> {
    pub(crate) fn render(&self, roots: &[NodeR<'_>]) -> String {
        let mut blocks = Vec::new();
        for root in roots {
            if root.is_element() && self.acts_as_block(root) {
                self.block(*root, &mut blocks);
            } else {
                blocks.extend(self.blocks_of(*root));
            }
        }
        if self.skipper.only_main() {
            trim_leading_chrome(&mut blocks);
        }
        let mut out = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    /// 公式节点（页面里不可能有公式时直接跳过检查）。
    fn math(&self, node: &NodeR<'_>) -> Option<Math> {
        if self.skipper.has_math() {
            math::math_of(node)
        } else {
            None
        }
    }

    fn skip(&self, node: &NodeR<'_>) -> bool {
        if node.is_comment() {
            return true;
        }
        // 公式在 `<math>`、`<script>`、隐藏的 span 里，照常的跳过规则会把它删掉
        node.is_element() && self.skipper.skip(node) && self.math(node).is_none()
    }

    /// 元素按块处理还是按行内处理。未知元素（自定义元素等）看里面有没有块级内容。
    fn acts_as_block(&self, node: &NodeR<'_>) -> bool {
        match self.math(node) {
            Some(Math::Tex(_, display)) => return display,
            Some(Math::Skip) => return false,
            None => {}
        }
        match node.tag() {
            Some(t) if is_block_tag(t) => true,
            Some(t) if is_inline_tag(t) => self.skipper.has_block_descendant(node),
            Some(_) => self.skipper.has_block_descendant(node),
            None => false,
        }
    }
}

fn join_blocks(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 正文一级标题之前的站点框架：logo、站点名链接、"前往官网"、搜索框提示这类块，
/// 常常既不在 `<header>` 也不在 `<nav>` 里，跳过规则抓不到。只取正文时，如果前十几个块里有
/// 一级标题，就去掉它前面"像框架"的块（纯链接、纯图片、或很短又不成句）；标题块和成句的段落保留。
/// 评测集上噪声泄漏降 12%，召回基本不变。
fn trim_leading_chrome(blocks: &mut Vec<Block>) {
    let Some(h1) = blocks
        .iter()
        .take(12)
        .position(|b| b.text.starts_with("# "))
    else {
        return;
    };
    let mut i = 0;
    blocks.retain(|b| {
        let keep = i >= h1 || !looks_like_chrome(&b.text);
        i += 1;
        keep
    });
}

fn looks_like_chrome(text: &str) -> bool {
    // 标题是内容的组织结构，不当框架删
    if text.starts_with('#') {
        return false;
    }
    let plain = strip_links(&strip_images(text));
    let words = plain.split_whitespace().count();
    if words == 0 {
        return true;
    }
    let sentence = plain.contains(['.', '。', '!', '?', '！', '？']);
    !sentence && (words <= 4 || plain.chars().count() < 40)
}

/// 去掉 `![alt](src)` 图片。
fn strip_images(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        rest = after.find(')').map_or("", |j| &after[j + 1..]);
    }
    out.push_str(rest);
    out
}

/// 去掉 `[text](href)` 链接（连同链接文字），纯链接的块因此变空。
fn strip_links(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        match (after.find("]("), after.find(')')) {
            (Some(_), Some(j)) => rest = &after[j + 1..],
            _ => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}
