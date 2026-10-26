//! 关闭脚本（规范的 scripting flag 关掉）时的 `<noscript>`：body 里是普通元素，head 里按
//! "in head noscript" 插入模式只收 head 类元素。开启脚本（默认，和浏览器一致）时内容是原始文本。

use crate::Tag;

use super::{Builder, Mode, Next, is_ws};

impl<'a> Builder<'a> {
    /// 处在规范的 "in head noscript" 插入模式。
    pub(super) fn in_head_noscript(&self) -> bool {
        !self.scripting && self.mode == Mode::InHead && self.current_is(Tag::Noscript)
    }

    /// "in head noscript" 里的开始标签。返回 `None` 时 noscript 已关掉，调用方按 "in head" 继续。
    pub(super) fn head_noscript_start(&mut self, tag: Tag, name: (usize, usize)) -> Option<Next> {
        use Tag::*;
        match tag {
            Html => {
                self.merge_attrs(self.html);
                Some(Next::Data)
            }
            Basefont | Bgsound | Link | Meta | Noframes | Style => self.head_element(tag, name),
            Head | Noscript => {
                self.scratch.clear();
                Some(Next::Data)
            }
            _ => {
                self.pop();
                None
            }
        }
    }

    /// "in head noscript" 里的结束标签。返回 true 表示处理完了。
    pub(super) fn head_noscript_end(&mut self, tag: Tag) -> bool {
        match tag {
            Tag::Noscript => {
                self.pop();
                true
            }
            // `</br>` 按 "anything else"：关掉 noscript 后照常处理
            Tag::Br => {
                self.pop();
                false
            }
            _ => true,
        }
    }

    /// "in head noscript" 里的文字：开头的空白放进 noscript，后面还有别的字符就关掉 noscript，
    /// 返回剩下那段的起点。
    pub(super) fn head_noscript_text(&mut self, s: usize, e: usize) -> usize {
        let lead = self.src.as_bytes()[s..e]
            .iter()
            .take_while(|&&c| is_ws(c))
            .count();
        if lead > 0 {
            self.add_text(crate::dom::Span::source(s, s + lead), false);
        }
        if s + lead < e {
            self.pop();
        }
        s + lead
    }
}
