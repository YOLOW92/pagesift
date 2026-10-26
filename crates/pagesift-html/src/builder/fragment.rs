//! 片段解析（规范的 HTML fragment parsing algorithm）：在给定上下文元素里解析一段 HTML，
//! 结果是根 `<html>` 元素的子节点。
//!
//! 上下文元素不进树：栈里只有根元素时，[`Builder::current`] 把它当成上下文元素（规范的
//! "adjusted current node"），插入模式也由它推断；根元素不出栈。

use crate::Tag;
use crate::dom::{Data, Namespace, Span};

use super::{Builder, Mode, Ns, Open, foreign_kind};

/// 在上下文元素 `name`（命名空间 `ns`）里解析 `src`。
pub(super) fn run(src: &str, tb: &mut Builder<'_>, ns: Namespace, name: &str) {
    let lname = name.to_ascii_lowercase();
    let tag = match ns {
        Namespace::Html => Tag::from_name(&lname),
        Namespace::Svg if lname == "svg" => Tag::Svg,
        Namespace::MathMl if lname == "math" => Tag::Math,
        _ => Tag::Other,
    };
    let kind = match ns {
        Namespace::Html => Ns::Html,
        _ => match tag {
            Tag::Svg | Tag::Math => Ns::Foreign,
            _ => foreign_kind(ns, &lname, false),
        },
    };
    // 片段所在的文档按 no-quirks 处理
    tb.quirks = false;
    tb.html = tb.new_node(Data::Element {
        tag: Tag::Html,
        ns: Namespace::Html,
        name: Span::default(),
        attrs: 0,
        attrs_len: 0,
    });
    tb.append(0, tb.html);
    tb.stack.push(Open {
        id: tb.html,
        tag: Tag::Html,
        ns: Ns::Html,
    });
    tb.floor = 1;
    tb.context = Some(Open {
        id: tb.html,
        tag,
        ns: kind,
    });
    tb.context_ns = ns;
    // 规范的 "reset the insertion mode appropriately"，以上下文元素为最后一个节点
    tb.mode = match tag {
        _ if ns != Namespace::Html => Mode::InBody,
        Tag::Html => Mode::BeforeHead,
        Tag::Frameset => Mode::InFrameset,
        _ => Mode::InBody,
    };
    if ns == Namespace::Html {
        // 分词器的初始状态：这些上下文里没有"合适的结束标签"，整段都是文本
        let decode = match tag {
            Tag::Title | Tag::Textarea => Some(true),
            Tag::Style | Tag::Xmp | Tag::Iframe | Tag::Noembed | Tag::Noframes | Tag::Script => {
                Some(false)
            }
            Tag::Noscript if tb.scripting => Some(false),
            Tag::Plaintext => Some(false),
            _ => None,
        };
        if let Some(decode) = decode {
            tb.raw_text(0, src.len(), decode);
            return;
        }
    }
    super::tokenize(src, tb);
}

impl<'a> Builder<'a> {
    /// 片段解析时栈里只剩根元素（当前节点是上下文元素）。
    pub(super) fn at_fragment_root(&self) -> bool {
        self.context.is_some() && self.stack.len() == 1
    }

    /// 表格结构标签找不到打开的表格时：片段的上下文是表格（主体、行）就以根元素为表格，
    /// 返回它在栈里的位置。
    pub(super) fn fragment_table_root(&self, tag: Tag) -> Option<usize> {
        use Tag::*;
        let ctx = self.context?.tag;
        let ok = match ctx {
            Table => true,
            Colgroup => tag == Col,
            Tbody | Thead | Tfoot => matches!(tag, Tr | Td | Th),
            Tr => matches!(tag, Td | Th),
            _ => false,
        };
        (ok && self.context?.ns == Ns::Html).then_some(0)
    }

    /// 栈里只剩根元素时，上下文元素的插入模式要忽略的开始标签（规范 "in column group"、
    /// "in table"、"in caption"、"in select" 里"没有对应元素在作用域里就忽略"的那些）。
    pub(super) fn fragment_root_start(&mut self, tag: Tag) -> Option<super::Next> {
        use Tag::*;
        let c = self.context?;
        if c.ns != Ns::Html {
            return None;
        }
        let ignore = match c.tag {
            Colgroup => !matches!(tag, Col | Template),
            Table | Tbody | Thead | Tfoot | Tr => tag == Table,
            Caption => matches!(
                tag,
                Caption | Col | Colgroup | Tbody | Td | Tfoot | Th | Thead | Tr
            ),
            Select => matches!(tag, Input | Select),
            _ => false,
        };
        ignore.then(|| {
            self.scratch.clear();
            super::Next::Data
        })
    }

    /// 上下文是列组：文字里只有空白有效。
    pub(super) fn fragment_whitespace_only(&self) -> bool {
        self.context
            .is_some_and(|c| c.tag == Tag::Colgroup && c.ns == Ns::Html)
    }
}
