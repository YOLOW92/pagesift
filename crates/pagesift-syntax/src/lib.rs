//! 与 tree-sitter 语法兼容的纯 Rust 解析运行时与代码大纲提取（基于 [`yolowsyntax`]）。
//!
//! 语法表与外部扫描器由 [`yolowsyntax`] 提供；来源和许可见 `THIRD-PARTY.md`。

#![forbid(unsafe_code)]

pub use yolowsyntax::{
    Limits, Outline, Section, Symbol, SymbolKind, compact, extract, langs, lexer, parser, recover,
    scanner, stack, table, tree,
};
#[cfg(feature = "tools")]
pub use yolowsyntax::{cdata, convert, corpus, lexconv};

/// 安全解析入口：对底层转译语法表与外部扫描器补充 `catch_unwind` 隔离，异常时降级为 `truncated` 退化树。
pub mod api {
    pub use yolowsyntax::api::{Ev, ParseTree, VNode};

    pub fn parse(
        lang: &yolowsyntax::table::Language,
        input: &[u8],
        limits: yolowsyntax::Limits,
    ) -> ParseTree {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            yolowsyntax::api::parse(lang, input, limits)
        }))
        .unwrap_or_else(|_| {
            yolowsyntax::api::parse(
                lang,
                input,
                yolowsyntax::Limits {
                    max_nodes: 0,
                    ..limits
                },
            )
        })
    }
}

/// 提取代码大纲，并保证所有 `Section` 范围严格对齐在 `source` 的 UTF-8 字符边界上且首尾相接覆盖全文。
pub fn outline(tag: &str, source: &str) -> Option<Outline> {
    let mut out = yolowsyntax::outline(tag, source)?;
    sanitize_sections(source, &mut out.sections);
    Some(out)
}

fn ceil_utf8_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn sanitize_sections(source: &str, sections: &mut Vec<Section>) {
    let len = source.len();
    if sections.is_empty() {
        sections.push(Section {
            start: 0,
            end: len,
            title: None,
        });
        return;
    }
    let mut cursor = 0usize;
    let mut sanitized = Vec::with_capacity(sections.len());
    for sec in sections.drain(..) {
        let end = ceil_utf8_boundary(source, sec.end);
        if end <= cursor {
            continue;
        }
        sanitized.push(Section {
            start: cursor,
            end,
            title: sec.title,
        });
        cursor = end;
    }
    match sanitized.last_mut() {
        Some(last) => last.end = len,
        None => sanitized.push(Section {
            start: 0,
            end: len,
            title: None,
        }),
    }
    *sections = sanitized;
}
