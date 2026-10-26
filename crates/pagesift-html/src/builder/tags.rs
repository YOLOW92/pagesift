use crate::Tag;
use crate::dom::{Data, NONE, Span};

use super::{
    Builder, MAX_DEPTH, Mode, Next, Ns, Open, Scope, breaks_out_of_foreign, heading,
    integration_point, table_context,
};

impl<'a> Builder<'a> {
    pub(super) fn start_tag(&mut self, name: (usize, usize), self_closing: bool) -> Next {
        self.skip_lf = false;
        let tag = self.tag_of(name);
        if self.mode == Mode::Initial {
            if tag == Tag::Html {
                self.ensure_html();
                self.merge_attrs(self.html);
                return Next::Data;
            }
            self.ensure_html();
        }
        if self.mode == Mode::BeforeHead {
            match tag {
                Tag::Html => {
                    self.merge_attrs(self.html);
                    return Next::Data;
                }
                Tag::Head => {
                    self.head = self.create(Tag::Head, name);
                    self.append(self.html, self.head);
                    self.stack.push(Open {
                        id: self.head,
                        tag: Tag::Head,
                        ns: Ns::Html,
                    });
                    self.mode = Mode::InHead;
                    return Next::Data;
                }
                _ => self.ensure_head(),
            }
        }
        if self.mode == Mode::InHead && self.in_template() && self.head_element_tag(tag).is_none() {
            return self.body_start_tag(tag, name, self_closing);
        }
        if self.mode == Mode::InHead {
            if let Some(next) = self.head_element(tag, name) {
                return next;
            }
            if tag == Tag::Head {
                return Next::Data;
            }
            self.close_head();
        }
        if self.mode == Mode::AfterHead {
            match tag {
                Tag::Html => {
                    self.merge_attrs(self.html);
                    return Next::Data;
                }
                Tag::Body => {
                    self.body = self.create(Tag::Body, name);
                    self.append(self.html, self.body);
                    self.stack.push(Open {
                        id: self.body,
                        tag: Tag::Body,
                        ns: Ns::Html,
                    });
                    self.mode = Mode::InBody;
                    return Next::Data;
                }
                Tag::Head => return Next::Data,
                _ => {}
            }
            // head 之后、body 之前的 head 类元素仍然放进 head
            let head_like = matches!(
                tag,
                Tag::Base
                    | Tag::Basefont
                    | Tag::Bgsound
                    | Tag::Link
                    | Tag::Meta
                    | Tag::Noframes
                    | Tag::Script
                    | Tag::Style
                    | Tag::Template
                    | Tag::Title
            );
            if head_like && self.head != NONE {
                self.stack.push(Open {
                    id: self.head,
                    tag: Tag::Head,
                    ns: Ns::Html,
                });
                let next = self.head_element(tag, name).unwrap_or(Next::Data);
                if let Some(i) = self.stack.iter().rposition(|o| o.id == self.head) {
                    self.stack.remove(i);
                }
                return next;
            }
            self.ensure_body();
        }
        self.body_start_tag(tag, name, self_closing)
    }

    pub(super) fn head_element_tag(&self, tag: Tag) -> Option<()> {
        matches!(
            tag,
            Tag::Html
                | Tag::Base
                | Tag::Basefont
                | Tag::Bgsound
                | Tag::Link
                | Tag::Meta
                | Tag::Title
                | Tag::Noscript
                | Tag::Noframes
                | Tag::Style
                | Tag::Script
                | Tag::Template
        )
        .then_some(())
    }

    /// head 里允许的元素；不是的返回 `None`。
    pub(super) fn head_element(&mut self, tag: Tag, name: (usize, usize)) -> Option<Next> {
        let next = match tag {
            Tag::Html => {
                self.merge_attrs(self.html);
                Next::Data
            }
            Tag::Base | Tag::Basefont | Tag::Bgsound | Tag::Link | Tag::Meta => {
                self.insert(tag, name, Ns::Html, false, false);
                Next::Data
            }
            Tag::Title => {
                self.insert(tag, name, Ns::Html, true, false);
                Next::RcData
            }
            Tag::Noscript | Tag::Noframes | Tag::Style | Tag::Script => {
                self.insert(tag, name, Ns::Html, true, false);
                Next::RawText
            }
            Tag::Template => {
                self.insert(tag, name, Ns::Html, true, false);
                Next::Data
            }
            _ => return None,
        };
        Some(next)
    }

    pub(super) fn body_start_tag(
        &mut self,
        tag: Tag,
        name: (usize, usize),
        self_closing: bool,
    ) -> Next {
        // 规范：HTML 里的 `<image>` 当作 `<img>`；SVG 里的 `<image>` 是 SVG 元素，不能触发跳出
        let tag = if tag == Tag::Other
            && !self.in_foreign()
            && self.src[name.0..name.1].eq_ignore_ascii_case("image")
        {
            Tag::Img
        } else {
            tag
        };
        if self.in_foreign() {
            let font_breaks = tag == Tag::Font
                && self.scratch.iter().any(|&((s, e), _)| {
                    let n = &self.src[s..e];
                    ["color", "face", "size"]
                        .iter()
                        .any(|k| n.eq_ignore_ascii_case(k))
                });
            if breaks_out_of_foreign(tag) || font_breaks {
                while self.in_foreign() {
                    self.pop();
                }
            } else {
                self.insert_foreign(tag, name, self_closing);
                return Next::Data;
            }
        }
        use Tag::*;
        match tag {
            Html => self.merge_attrs(self.html),
            Body => self.merge_attrs(self.body),
            Head | Frameset | Frame => {}
            Base | Basefont | Bgsound | Link | Meta | Param | Source | Track => {
                self.insert_html(tag, name)
            }
            Area | Br | Embed | Img | Keygen | Wbr | Input => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Hr => {
                self.close_p();
                self.insert_html(tag, name);
            }
            Title => {
                self.insert_html(tag, name);
                return Next::RcData;
            }
            Textarea => {
                self.insert_html(tag, name);
                self.skip_lf = true;
                return Next::RcData;
            }
            Xmp => {
                self.close_p();
                self.insert_html(tag, name);
                return Next::RawText;
            }
            Style | Noframes | Noscript | Iframe | Noembed | Script => {
                self.insert_html(tag, name);
                return Next::RawText;
            }
            Plaintext => {
                self.close_p();
                self.insert_html(tag, name);
                return Next::PlainText;
            }
            Address | Article | Aside | Blockquote | Center | Details | Dialog | Dir | Div | Dl
            | Fieldset | Figcaption | Figure | Footer | Header | Hgroup | Main | Menu | Nav
            | Ol | P | Search | Section | Summary | Ul => {
                self.close_p();
                self.insert_html(tag, name);
            }
            Form => {
                if self.form != NONE && !self.in_template() {
                    self.scratch.clear();
                    return Next::Data;
                }
                if self
                    .current()
                    .is_some_and(|o| o.ns == Ns::Html && table_context(o.tag))
                {
                    // 表格里的 form 是个空元素，内容不进 form
                    self.form = self.insert(tag, name, Ns::Html, false, false);
                } else {
                    self.close_p();
                    self.form = self.insert(tag, name, Ns::Html, true, true);
                }
            }
            H1 | H2 | H3 | H4 | H5 | H6 => {
                self.close_p();
                if self
                    .current()
                    .is_some_and(|o| o.ns == Ns::Html && heading(o.tag))
                {
                    self.pop();
                }
                self.insert_html(tag, name);
            }
            Pre | Listing => {
                self.close_p();
                self.insert_html(tag, name);
                self.skip_lf = true;
            }
            Li => {
                self.close_list_item(&[Li]);
                self.close_p();
                self.insert_html(tag, name);
            }
            Dd | Dt => {
                self.close_list_item(&[Dd, Dt]);
                self.close_p();
                self.insert_html(tag, name);
            }
            Button => {
                if let Some(i) = self.in_scope(Button, Scope::Default) {
                    self.pop_to(i);
                }
                self.insert_html(tag, name);
            }
            A => {
                // 上一个 `<a>` 没关：先按 `</a>` 处理
                let start = self.afe_after_marker();
                if let Some(old) = self.afe[start..]
                    .iter()
                    .copied()
                    .find(|&e| self.node_tag(e) == A)
                {
                    let attrs = std::mem::take(&mut self.scratch);
                    self.adoption_agency(A);
                    self.scratch = attrs;
                    self.afe.retain(|&e| e != old);
                    if let Some(i) = self.on_stack(old) {
                        self.stack.remove(i);
                    }
                }
                self.reconstruct();
                let id = self.insert(tag, name, Ns::Html, true, true);
                self.push_formatting(id);
            }
            Nobr => {
                self.reconstruct();
                if self.in_scope(Nobr, Scope::Default).is_some() {
                    let attrs = std::mem::take(&mut self.scratch);
                    self.adoption_agency(Nobr);
                    self.scratch = attrs;
                    self.reconstruct();
                }
                let id = self.insert(tag, name, Ns::Html, true, true);
                self.push_formatting(id);
            }
            B | Big | Code | Em | Font | I | S | Small | Strike | Strong | Tt | U => {
                self.reconstruct();
                let id = self.insert(tag, name, Ns::Html, true, true);
                self.push_formatting(id);
            }
            Applet | Marquee | Object => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Select => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Table => {
                let in_table = self
                    .current()
                    .is_some_and(|o| o.ns == Ns::Html && table_context(o.tag));
                if in_table && let Some(i) = self.in_scope(Table, Scope::Table) {
                    self.pop_to(i);
                } else {
                    self.close_p();
                }
                self.insert(tag, name, Ns::Html, true, false);
            }
            Caption | Colgroup | Col | Tbody | Thead | Tfoot | Tr | Td | Th => {
                self.table_part(tag, name)
            }
            Option | Optgroup => {
                if self.current_is(Option) {
                    self.pop();
                }
                self.reconstruct();
                self.insert_html(tag, name);
            }
            Rb | Rtc => {
                if self.in_scope(Ruby, Scope::Default).is_some() {
                    self.pop_implied(false);
                }
                self.insert_html(tag, name);
            }
            Rp | Rt => {
                if self.in_scope(Ruby, Scope::Default).is_some() {
                    self.pop_implied(true);
                }
                self.insert_html(tag, name);
            }
            Svg | Math => {
                self.reconstruct();
                let id = self.create(tag, name);
                self.place(id, true);
                if !self_closing && self.stack.len() < MAX_DEPTH {
                    self.stack.push(Open {
                        id,
                        tag,
                        ns: Ns::Foreign,
                    });
                }
            }
            _ => {
                self.reconstruct();
                self.insert_html(tag, name);
            }
        }
        Next::Data
    }

    /// SVG/MathML 里的元素：名字照存，自闭合的不入栈。
    pub(super) fn insert_foreign(&mut self, tag: Tag, name: (usize, usize), self_closing: bool) {
        let tag = if matches!(tag, Tag::Svg | Tag::Math) {
            tag
        } else {
            Tag::Other
        };
        let id = self.create(tag, name);
        self.place(id, false);
        if self_closing || self.stack.len() >= MAX_DEPTH {
            return;
        }
        let lname = self.src[name.0..name.1].to_ascii_lowercase();
        let ns = if integration_point(&lname) {
            Ns::Integration
        } else {
            Ns::Foreign
        };
        self.stack.push(Open { id, tag, ns });
    }

    /// 表格结构元素：补上省略的 tbody、tr，关掉未闭合的单元格和行。不在表格里时忽略。
    pub(super) fn table_part(&mut self, tag: Tag, name: (usize, usize)) {
        use Tag::*;
        let Some(table) = self.in_scope(Table, Scope::Table) else {
            self.scratch.clear();
            return;
        };
        let clear_to_table = |b: &mut Self| b.pop_to(table + 1);
        match tag {
            Caption | Colgroup | Tbody | Thead | Tfoot => {
                clear_to_table(self);
                self.insert(tag, name, Ns::Html, true, false);
            }
            Col => {
                if !self.current_is(Colgroup) {
                    clear_to_table(self);
                    self.implied(Colgroup);
                }
                self.insert(tag, name, Ns::Html, false, false);
            }
            Tr | Td | Th => {
                if let Some(i) = self.find_in_scope(|t| matches!(t, Td | Th), Scope::Table) {
                    self.pop_to(i);
                }
                // 清到行（单元格）或表格主体（行）的上下文，缺的 tbody、tr 补上
                let stop: &[Tag] = if tag == Tr {
                    &[Tbody, Thead, Tfoot, Table]
                } else {
                    &[Tr, Tbody, Thead, Tfoot, Table]
                };
                while let Some(o) = self.current() {
                    if o.ns == Ns::Html && stop.contains(&o.tag) {
                        break;
                    }
                    self.pop();
                }
                if self.current_is(Table) {
                    self.implied(Tbody);
                }
                if tag != Tr && !self.current_is(Tr) {
                    self.implied(Tr);
                }
                self.insert(tag, name, Ns::Html, true, false);
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn implied(&mut self, tag: Tag) {
        let id = self.new_node(Data::Element {
            tag,
            name: Span::default(),
            attrs: self.attrs.len() as u32,
            attrs_len: 0,
        });
        self.place(id, false);
        self.stack.push(Open {
            id,
            tag,
            ns: Ns::Html,
        });
    }
}
