use crate::Tag;
use crate::dom::{Data, NONE};

use super::{Builder, Mode, Ns, Scope, formatting, heading, special, void};

impl<'a> Builder<'a> {
    pub(super) fn end_tag(&mut self, name: (usize, usize)) {
        self.skip_lf = false;
        self.scratch.clear();
        let tag = self.tag_of(name);
        if self.in_frameset_modes() {
            self.frameset_end_tag(tag);
            return;
        }
        // 外部内容里的 `</p>`、`</br>` 跳出 SVG/MathML，按 HTML 规则处理
        if self.in_foreign() && matches!(tag, Tag::P | Tag::Br) {
            while self.in_foreign() {
                self.pop();
            }
        }
        // 外部内容：按名字（不分大小写）在外部元素里找，碰到 HTML 元素就按 HTML 规则处理
        if self.stack.last().is_some_and(|o| o.ns != Ns::Html) {
            let raw = &self.src[name.0..name.1];
            for i in (0..self.stack.len()).rev() {
                let o = self.stack[i];
                if o.ns == Ns::Html {
                    break;
                }
                if self.element_name_eq(o.id, raw) {
                    self.pop_to(i);
                    return;
                }
            }
        }
        // 最常见的情况：关的就是当前元素
        if let Some(cur) = self.current()
            && !cur.ns.foreign()
            && cur.tag == tag
            && (tag != Tag::Other || self.element_name_eq(cur.id, &self.src[name.0..name.1]))
            && !matches!(tag, Tag::Html | Tag::Body)
            && !formatting(tag)
        {
            if cur.id == self.head {
                self.mode = Mode::AfterHead;
            }
            if cur.id == self.form {
                self.form = NONE;
            }
            self.pop();
            return;
        }
        // 列组里只能有 `<col>`：别的结束标签先关掉列组
        if self.current_is(Tag::Colgroup)
            && !matches!(tag, Tag::Colgroup | Tag::Col | Tag::Template)
        {
            self.pop();
        }
        use Tag::*;
        match self.mode {
            Mode::InBody => {}
            _ => match tag {
                Head if self.mode == Mode::InHead => {
                    self.close_head();
                    return;
                }
                // head 里的 template
                Template => {}
                Body | Html | Br => {
                    self.ensure_body();
                    if tag != Br {
                        return;
                    }
                }
                _ => return,
            },
        }
        match tag {
            Body | Html | Head => {}
            // `</form>` 只把 form 从栈里拿掉，里面没闭合的元素继续开着
            Form => {
                let form = std::mem::replace(&mut self.form, NONE);
                if let Some(i) = self.in_scope(Form, Scope::Default)
                    && self.stack[i].id == form
                {
                    self.stack.remove(i);
                }
            }
            t if formatting(t) => {
                if !self.adoption_agency(tag) {
                    self.any_other_end(tag, name);
                }
            }
            Br => {
                self.frameset_ok = false;
                self.reconstruct();
                self.insert_html(Br, name);
            }
            P => match self.in_scope(P, Scope::Button) {
                Some(i) => self.pop_to(i),
                // 没有打开的 `<p>`：规范要求补一个空段落
                None => {
                    self.insert(P, name, Ns::Html, false, true);
                }
            },
            Li => {
                if let Some(i) = self.in_scope(Li, Scope::ListItem) {
                    self.pop_to(i);
                }
            }
            H1 | H2 | H3 | H4 | H5 | H6 => {
                if let Some(i) = self.find_in_scope(heading, Scope::Default) {
                    self.pop_to(i);
                }
            }
            Table | Tbody | Thead | Tfoot | Tr | Td | Th | Caption | Colgroup => {
                if let Some(i) = self.in_scope(tag, Scope::Table) {
                    self.pop_to(i);
                }
            }
            // `</template>` 不看作用域：栈里有 template 就一直关到它（表格等边界挡不住）
            Template => {
                if let Some(i) = self
                    .stack
                    .iter()
                    .rposition(|o| o.tag == Template && o.ns == Ns::Html)
                {
                    self.pop_to(i);
                }
            }
            // 不是“特殊”元素，但结束标签按块级元素的规则关（中间没闭合的 `<p>` 等一起关掉）
            Dialog => {
                if let Some(i) = self.in_scope(Dialog, Scope::Default) {
                    self.pop_to(i);
                }
            }
            t if void(t) => {}
            t if special(t) => {
                if let Some(i) = self.in_scope(t, Scope::Default) {
                    self.pop_to(i);
                }
            }
            _ => self.any_other_end(tag, name),
        }
    }

    /// 一般的结束标签：从栈顶往下找同名元素，碰到"特殊"元素就放弃。
    pub(super) fn any_other_end(&mut self, tag: Tag, name: (usize, usize)) {
        let raw = &self.src[name.0..name.1];
        for i in (0..self.stack.len()).rev() {
            let o = self.stack[i];
            let same = o.tag == tag && (tag != Tag::Other || self.element_name_eq(o.id, raw));
            if same && !o.ns.foreign() {
                self.pop_to(i);
                return;
            }
            let stop = match o.ns {
                Ns::Html => special(o.tag),
                Ns::Integration | Ns::Annotation => true,
                Ns::Foreign => false,
            };
            if stop {
                return;
            }
        }
    }

    pub(super) fn element_name_eq(&self, id: u32, raw: &str) -> bool {
        match self.nodes[id as usize].data {
            Data::Element {
                tag: Tag::Other,
                name,
                ..
            } => self.str(name).eq_ignore_ascii_case(raw),
            Data::Element { tag, .. } => tag.name().eq_ignore_ascii_case(raw),
            _ => false,
        }
    }
}
