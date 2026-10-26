//! frameset 文档（规范的 "in frameset"、"after frameset"、"after after frameset" 插入模式）。
//!
//! `<frameset>` 出现在 body 有内容之前（frameset-ok 标志还没被清掉）时取代 body；之后只有
//! `<frameset>`、`<frame>`、`<noframes>` 和空白、注释有效，其余内容按规范丢弃。

use crate::Tag;

use super::{Builder, Mode, Next, Ns, Open, is_ws};

impl<'a> Builder<'a> {
    /// 当前处在 frameset 的三种插入模式之一。
    pub(super) fn in_frameset_modes(&self) -> bool {
        self.mode > Mode::InBody
    }

    /// 让 frameset-ok 变成 not ok 的开始标签（规范 "in body" 里逐条列出的那些，加上按 "in head"
    /// 规则处理的 `<template>`）。
    pub(super) fn frameset_ok_after(&mut self, tag: Tag) {
        use Tag::*;
        if self.frameset_ok
            && matches!(
                tag,
                Pre | Listing
                    | Li
                    | Dd
                    | Dt
                    | Button
                    | Applet
                    | Marquee
                    | Object
                    | Table
                    | Area
                    | Br
                    | Embed
                    | Img
                    | Keygen
                    | Wbr
                    | Input
                    | Hr
                    | Textarea
                    | Xmp
                    | Iframe
                    | Select
                    | Template
            )
            && !(tag == Input && self.hidden_input())
        {
            self.frameset_ok = false;
        }
    }

    /// "after head" 里的 `<frameset>`：直接插入。
    pub(super) fn insert_frameset(&mut self, name: (usize, usize)) {
        let id = self.create(Tag::Frameset, name);
        self.append(self.html, id);
        self.stack.push(Open {
            id,
            tag: Tag::Frameset,
            ns: Ns::Html,
        });
        self.mode = Mode::InFrameset;
    }

    /// "in body" 里的 `<frameset>`：body 还没有内容时取代 body，否则忽略。
    pub(super) fn frameset_in_body(&mut self, name: (usize, usize)) {
        let replaces = self.frameset_ok
            && self.stack.len() >= 2
            && self.stack[1].id == self.body
            && !self.in_template();
        if !replaces {
            self.scratch.clear();
            return;
        }
        self.detach(self.body);
        self.pop_to(1);
        self.insert_frameset(name);
    }

    /// frameset 三种模式下的开始标签。
    pub(super) fn frameset_start_tag(&mut self, tag: Tag, name: (usize, usize)) -> Next {
        match tag {
            Tag::Html => self.merge_attrs(self.html),
            Tag::Frameset if self.mode == Mode::InFrameset => {
                self.insert(tag, name, Ns::Html, true, false);
            }
            Tag::Frame if self.mode == Mode::InFrameset => {
                self.insert(tag, name, Ns::Html, false, false);
            }
            Tag::Noframes => {
                self.insert(tag, name, Ns::Html, true, false);
                return Next::RawText;
            }
            _ => self.scratch.clear(),
        }
        Next::Data
    }

    /// frameset 三种模式下的结束标签。
    pub(super) fn frameset_end_tag(&mut self, tag: Tag) {
        // `<noframes>` 是原始文本元素，它的结束标签关掉它
        if self.current_is(Tag::Noframes) {
            if tag == Tag::Noframes {
                self.pop();
            }
            return;
        }
        match (self.mode, tag) {
            (Mode::InFrameset, Tag::Frameset) => {
                if self.stack.len() > 1 {
                    self.pop();
                }
                if !self.current_is(Tag::Frameset) {
                    self.mode = Mode::AfterFrameset;
                }
            }
            (Mode::AfterFrameset, Tag::Html) => self.mode = Mode::AfterAfterFrameset,
            _ => {}
        }
    }

    /// frameset 三种模式下的文字：只留空白字符，其余丢弃。
    pub(super) fn frameset_text(&mut self, s: usize, e: usize) {
        let raw = &self.src.as_bytes()[s..e];
        if raw.iter().all(|&c| is_ws(c)) {
            let span = crate::dom::Span::source(s, e);
            self.add_text(span, false);
            return;
        }
        let ws: String = raw
            .iter()
            .filter(|&&c| is_ws(c))
            .map(|&c| c as char)
            .collect();
        if !ws.is_empty() {
            let start = self.buf.len();
            self.buf.push_str(&ws);
            self.add_text(crate::dom::Span::owned(start, self.buf.len()), false);
        }
    }
}
