use crate::Tag;
use crate::dom::{Data, NONE, Namespace, Span};

use super::{Builder, MAX_DEPTH, Mode, Ns, Open, Scope, special, table_context};

impl<'a> Builder<'a> {
    pub(super) fn afe_after_marker(&self) -> usize {
        self.afe
            .iter()
            .rposition(|&e| e == NONE)
            .map_or(0, |i| i + 1)
    }

    pub(super) fn push_formatting(&mut self, id: u32) {
        let start = self.afe_after_marker();
        let same: Vec<usize> = (start..self.afe.len())
            .filter(|&i| self.same_element(self.afe[i], id))
            .collect();
        if same.len() >= 3 {
            self.afe.remove(same[0]);
        }
        self.afe.push(id);
    }

    /// 重建活动格式元素：`<p><b>x<p>y` 里 y 也是粗体。
    pub(super) fn reconstruct(&mut self) {
        let Some(&last) = self.afe.last() else {
            return;
        };
        if last == NONE || self.on_stack(last).is_some() || self.stack.len() >= MAX_DEPTH {
            return;
        }
        let mut i = self.afe.len() - 1;
        while i > 0 {
            let e = self.afe[i - 1];
            if e == NONE || self.on_stack(e).is_some() {
                break;
            }
            i -= 1;
        }
        for j in i..self.afe.len() {
            let new = self.clone_element(self.afe[j]);
            self.place(new, true);
            let tag = self.node_tag(new);
            self.stack.push(Open {
                id: new,
                tag,
                ns: Ns::Html,
            });
            self.afe[j] = new;
        }
    }

    pub(super) fn special_entry(o: Open) -> bool {
        match o.ns {
            Ns::Html => special(o.tag),
            Ns::Integration | Ns::Annotation => true,
            Ns::Foreign => false,
        }
    }

    /// Adoption agency：格式元素的结束标签。`<a><div>x</a>y` 变成
    /// `<a></a><div><a>x</a>y</div>`，链接只包住它本来包住的文字。
    /// 返回 false 时调用方按普通结束标签处理。
    pub(super) fn adoption_agency(&mut self, tag: Tag) -> bool {
        if let Some(cur) = self.current()
            && cur.ns == Ns::Html
            && cur.tag == tag
            && !self.afe.contains(&cur.id)
        {
            self.pop();
            return true;
        }
        for _ in 0..8 {
            let start = self.afe_after_marker();
            let Some(fe_pos) = (start..self.afe.len())
                .rev()
                .find(|&i| self.node_tag(self.afe[i]) == tag)
            else {
                return false;
            };
            let fe = self.afe[fe_pos];
            let Some(fe_idx) = self.on_stack(fe) else {
                self.afe.remove(fe_pos);
                return true;
            };
            if self.stack[fe_idx + 1..]
                .iter()
                .any(|&o| self.boundary(o, Scope::Default))
            {
                return true;
            }
            let Some(fb_idx) =
                (fe_idx + 1..self.stack.len()).find(|&i| Self::special_entry(self.stack[i]))
            else {
                self.pop_to(fe_idx);
                self.afe.remove(fe_pos);
                return true;
            };
            let fb = self.stack[fb_idx].id;
            let common = self.stack[fe_idx - 1].id;
            let mut bookmark = fe_pos;
            let mut node_idx = fb_idx;
            let mut last = fb;
            let mut counter = 0;
            loop {
                counter += 1;
                node_idx -= 1;
                let node = self.stack[node_idx].id;
                if node == fe {
                    break;
                }
                let mut pos = self.afe.iter().position(|&e| e == node);
                if counter > 3
                    && let Some(p) = pos
                {
                    self.afe.remove(p);
                    if p < bookmark {
                        bookmark -= 1;
                    }
                    pos = None;
                }
                let Some(p) = pos else {
                    self.stack.remove(node_idx);
                    continue;
                };
                let new = self.clone_element(node);
                self.afe[p] = new;
                self.stack[node_idx].id = new;
                if last == fb {
                    bookmark = p + 1;
                }
                self.detach(last);
                self.append(new, last);
                last = new;
            }
            self.detach(last);
            if table_context(self.node_tag(common)) {
                self.foster(last, fe_idx);
            } else {
                self.append(common, last);
            }
            let e = self.clone_element(fe);
            self.move_children(fb, e);
            self.append(fb, e);
            if let Some(p) = self.afe.iter().position(|&x| x == fe) {
                self.afe.remove(p);
                if p < bookmark {
                    bookmark -= 1;
                }
            }
            self.afe.insert(bookmark.min(self.afe.len()), e);
            if let Some(i) = self.on_stack(fe) {
                self.stack.remove(i);
            }
            let at = self.on_stack(fb).map_or(self.stack.len(), |i| i + 1);
            self.stack.insert(
                at,
                Open {
                    id: e,
                    tag,
                    ns: Ns::Html,
                },
            );
        }
        true
    }

    pub(super) fn finish(&mut self) {
        if self.body == NONE && !self.in_frameset_modes() {
            self.ensure_body();
        }
    }

    pub(super) fn in_template(&self) -> bool {
        self.stack
            .iter()
            .any(|o| o.tag == Tag::Template && o.ns == Ns::Html)
    }

    pub(super) fn close_p(&mut self) {
        if let Some(i) = self.in_scope(Tag::P, Scope::Button) {
            self.pop_to(i);
        }
    }

    /// `<li>`、`<dd>`、`<dt>` 关掉同类的未闭合元素，遇到其他"特殊"元素就停。
    pub(super) fn close_list_item(&mut self, same: &[Tag]) {
        for i in (0..self.stack.len()).rev() {
            let o = self.stack[i];
            if o.ns == Ns::Html && same.contains(&o.tag) {
                self.pop_to(i);
                return;
            }
            let stops = match o.ns {
                Ns::Html => special(o.tag) && !matches!(o.tag, Tag::Address | Tag::Div | Tag::P),
                Ns::Integration | Ns::Annotation => true,
                Ns::Foreign => false,
            };
            if stops {
                return;
            }
        }
    }

    /// 打开着的 select 里出现表单控件：先关掉 select（控件不能放在 select 里）。
    pub(super) fn close_select(&mut self) {
        if let Some(i) = self.in_scope(Tag::Select, Scope::Default) {
            self.pop_to(i);
        }
    }

    /// 表格里的 select（规范的 “in select in table”）：表格结构标签先关掉 select。
    pub(super) fn close_select_in_table(&mut self) {
        if let Some(i) = self.in_scope(Tag::Select, Scope::Default)
            && self.stack[..i]
                .iter()
                .any(|o| o.tag == Tag::Table && o.ns == Ns::Html)
        {
            self.pop_to(i);
        }
    }

    /// 规范的 “generate implied end tags”，`except` 那种元素不关。
    pub(super) fn implied_end_tags(&mut self, except: Option<Tag>) {
        while let Some(o) = self.current() {
            let implied = o.ns == Ns::Html
                && matches!(
                    o.tag,
                    Tag::Dd
                        | Tag::Dt
                        | Tag::Li
                        | Tag::Optgroup
                        | Tag::Option
                        | Tag::P
                        | Tag::Rb
                        | Tag::Rp
                        | Tag::Rt
                        | Tag::Rtc
                )
                && Some(o.tag) != except;
            if !implied {
                return;
            }
            self.pop();
        }
    }

    // ---- 文档结构

    pub(super) fn ensure_html(&mut self) {
        if self.html == NONE {
            self.html = self.new_node(Data::Element {
                tag: Tag::Html,
                ns: Namespace::Html,
                name: Span::default(),
                attrs: 0,
                attrs_len: 0,
            });
            self.append(0, self.html);
            self.stack.push(Open {
                id: self.html,
                tag: Tag::Html,
                ns: Ns::Html,
            });
        }
        if self.mode < Mode::BeforeHead {
            self.mode = Mode::BeforeHead;
        }
    }

    pub(super) fn ensure_head(&mut self) {
        self.ensure_html();
        if self.head == NONE {
            self.head = self.new_node(Data::Element {
                tag: Tag::Head,
                ns: Namespace::Html,
                name: Span::default(),
                attrs: 0,
                attrs_len: 0,
            });
            self.append(self.html, self.head);
            self.stack.push(Open {
                id: self.head,
                tag: Tag::Head,
                ns: Ns::Html,
            });
            self.mode = Mode::InHead;
        }
    }

    pub(super) fn close_head(&mut self) {
        if self.mode <= Mode::InHead {
            self.ensure_head();
            if let Some(i) = self.stack.iter().rposition(|o| o.id == self.head) {
                self.pop_to(i);
            }
            self.mode = Mode::AfterHead;
        }
    }

    pub(super) fn ensure_body(&mut self) {
        if self.mode < Mode::AfterHead {
            self.close_head();
        }
        if self.body == NONE {
            // 规范 "after head" 的 "anything else"：隐式的 body 把 frameset-ok 设回 ok
            self.frameset_ok = true;
            self.body = self.new_node(Data::Element {
                tag: Tag::Body,
                ns: Namespace::Html,
                name: Span::default(),
                attrs: 0,
                attrs_len: 0,
            });
            self.append(self.html, self.body);
            self.stack.push(Open {
                id: self.body,
                tag: Tag::Body,
                ns: Ns::Html,
            });
        }
        self.mode = Mode::InBody;
    }
}
