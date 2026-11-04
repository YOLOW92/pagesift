//! HCL 外部扫描器，移植自 tree-sitter-hcl 的 `src/scanner.c`（Apache-2.0）。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_space};

const QUOTED_TEMPLATE_START: usize = 0;
const QUOTED_TEMPLATE_END: usize = 1;
const TEMPLATE_LITERAL_CHUNK: usize = 2;
const TEMPLATE_INTERPOLATION_START: usize = 3;
const TEMPLATE_INTERPOLATION_END: usize = 4;
const TEMPLATE_DIRECTIVE_START: usize = 5;
const TEMPLATE_DIRECTIVE_END: usize = 6;
const HEREDOC_IDENTIFIER: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Interpolation = 0,
    Directive = 1,
    Quoted = 2,
    Heredoc = 3,
}

struct Frame {
    kind: Ctx,
    /// 只有 `Heredoc` 用。
    id: Vec<u8>,
}

#[derive(Default)]
pub struct Hcl {
    stack: Vec<Frame>,
}

impl Hcl {
    fn top_is(&self, k: Ctx) -> bool {
        self.stack.last().is_some_and(|f| f.kind == k)
    }

    fn push(&mut self, kind: Ctx) {
        self.stack.push(Frame {
            kind,
            id: Vec::new(),
        });
    }

    fn in_template(&self) -> bool {
        self.top_is(Ctx::Quoted) || self.top_is(Ctx::Heredoc)
    }
}

fn accept(lx: &mut ScanLexer, sym: usize) -> bool {
    lx.set_result(sym as u16);
    true
}

fn accept_and_advance(lx: &mut ScanLexer, sym: usize) -> bool {
    lx.next();
    accept(lx, sym)
}

fn consume_xdigit(lx: &mut ScanLexer) -> bool {
    lx.next();
    lx.lookahead().is_ascii_hexdigit()
}

impl Scanner for Hcl {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        let mut ws_newline = false;
        while is_space(lx.lookahead()) {
            if lx.lookahead() == '\n' {
                ws_newline = true;
            }
            lx.skip();
        }
        if lx.lookahead() == '\0' {
            return false;
        }
        if v[QUOTED_TEMPLATE_START] && !self.top_is(Ctx::Quoted) && lx.lookahead() == '"' {
            self.push(Ctx::Quoted);
            return accept_and_advance(lx, QUOTED_TEMPLATE_START);
        }
        if v[QUOTED_TEMPLATE_END] && self.top_is(Ctx::Quoted) && lx.lookahead() == '"' {
            self.stack.pop();
            return accept_and_advance(lx, QUOTED_TEMPLATE_END);
        }
        if v[TEMPLATE_INTERPOLATION_START]
            && v[TEMPLATE_LITERAL_CHUNK]
            && !self.top_is(Ctx::Interpolation)
            && lx.lookahead() == '$'
        {
            lx.next();
            if lx.lookahead() == '{' {
                self.push(Ctx::Interpolation);
                return accept_and_advance(lx, TEMPLATE_INTERPOLATION_START);
            }
            if lx.lookahead() == '$' {
                lx.next();
                if lx.lookahead() == '{' {
                    return accept_and_advance(lx, TEMPLATE_LITERAL_CHUNK);
                }
            }
            return accept(lx, TEMPLATE_LITERAL_CHUNK);
        }
        if v[TEMPLATE_INTERPOLATION_END] && self.top_is(Ctx::Interpolation) && lx.lookahead() == '}'
        {
            self.stack.pop();
            return accept_and_advance(lx, TEMPLATE_INTERPOLATION_END);
        }
        if v[TEMPLATE_DIRECTIVE_START]
            && v[TEMPLATE_LITERAL_CHUNK]
            && !self.top_is(Ctx::Directive)
            && lx.lookahead() == '%'
        {
            lx.next();
            if lx.lookahead() == '{' {
                self.push(Ctx::Directive);
                return accept_and_advance(lx, TEMPLATE_DIRECTIVE_START);
            }
            if lx.lookahead() == '%' {
                lx.next();
                if lx.lookahead() == '{' {
                    return accept_and_advance(lx, TEMPLATE_LITERAL_CHUNK);
                }
            }
            return accept(lx, TEMPLATE_LITERAL_CHUNK);
        }
        if v[TEMPLATE_DIRECTIVE_END] && self.top_is(Ctx::Directive) && lx.lookahead() == '}' {
            self.stack.pop();
            return accept_and_advance(lx, TEMPLATE_DIRECTIVE_END);
        }
        if v[HEREDOC_IDENTIFIER] && !self.top_is(Ctx::Heredoc) {
            let mut id = Vec::new();
            while lx.lookahead().is_alphanumeric() || matches!(lx.lookahead(), '_' | '-') {
                id.push(lx.lookahead() as u32 as u8);
                lx.next();
            }
            self.stack.push(Frame {
                kind: Ctx::Heredoc,
                id,
            });
            return accept(lx, HEREDOC_IDENTIFIER);
        }
        if v[HEREDOC_IDENTIFIER] && self.top_is(Ctx::Heredoc) && ws_newline {
            let expected = self.stack.last().map(|f| f.id.clone()).unwrap_or_default();
            for b in expected {
                if lx.lookahead() as u32 == b as u32 {
                    lx.next();
                } else {
                    return accept(lx, TEMPLATE_LITERAL_CHUNK);
                }
            }
            // 标识符独占一行才算结束
            lx.mark_end();
            while is_space(lx.lookahead()) && lx.lookahead() != '\n' {
                lx.next();
            }
            if lx.lookahead() == '\n' {
                self.stack.pop();
                return accept(lx, HEREDOC_IDENTIFIER);
            }
            lx.next();
            lx.mark_end();
            return accept(lx, TEMPLATE_LITERAL_CHUNK);
        }
        if v[TEMPLATE_LITERAL_CHUNK] && self.top_is(Ctx::Quoted) && lx.lookahead() == '\\' {
            lx.next();
            return match lx.lookahead() {
                '"' | 'n' | 'r' | 't' | '\\' => accept_and_advance(lx, TEMPLATE_LITERAL_CHUNK),
                'u' => {
                    for _ in 0..4 {
                        if !consume_xdigit(lx) {
                            return false;
                        }
                    }
                    accept_and_advance(lx, TEMPLATE_LITERAL_CHUNK)
                }
                'U' => {
                    for _ in 0..8 {
                        if !consume_xdigit(lx) {
                            return false;
                        }
                    }
                    accept_and_advance(lx, TEMPLATE_LITERAL_CHUNK)
                }
                _ => false,
            };
        }
        if v[TEMPLATE_LITERAL_CHUNK] && self.in_template() {
            return accept_and_advance(lx, TEMPLATE_LITERAL_CHUNK);
        }
        false
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        if self.stack.len() > 127 {
            return;
        }
        let base = out.len();
        out.extend_from_slice(&(self.stack.len() as u32).to_le_bytes());
        for f in &self.stack {
            if out.len() - base + 4 + 4 + f.id.len() >= MAX_SERIALIZED || f.id.len() > 127 {
                out.truncate(base);
                return;
            }
            out.extend_from_slice(&(f.kind as u32).to_le_bytes());
            out.extend_from_slice(&(f.id.len() as u32).to_le_bytes());
            out.extend_from_slice(&f.id);
        }
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.stack.clear();
        if d.len() < 4 {
            return;
        }
        let n = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
        let mut p = 4;
        for _ in 0..n {
            if p + 8 > d.len() {
                return;
            }
            let kind = match u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) {
                0 => Ctx::Interpolation,
                1 => Ctx::Directive,
                2 => Ctx::Quoted,
                _ => Ctx::Heredoc,
            };
            let len = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
            p += 8;
            let len = len.min(d.len() - p);
            self.stack.push(Frame {
                kind,
                id: d[p..p + len].to_vec(),
            });
            p += len;
        }
    }
}
