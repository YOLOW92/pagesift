//! Erlang 外部扫描器，移植自 tree-sitter-erlang 的 `src/scanner.c`
//! （Apache-2.0，Copyright (c) Meta Platforms, Inc. and affiliates）。无状态：序列化为空。

use super::{ScanLexer, Scanner};

const TQ_STRING: usize = 0;
const TQ_SIGIL_STRING: usize = 1;

#[derive(Default)]
pub struct Erlang;

fn is_whitespace(lx: &ScanLexer) -> bool {
    let c = lx.lookahead() as u32;
    (0x01..=0x20).contains(&c) || (0x80..=0xA0).contains(&c)
}

impl Scanner for Erlang {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if !(v[TQ_STRING] || v[TQ_SIGIL_STRING]) {
            return false;
        }
        while is_whitespace(lx) {
            lx.skip();
        }
        let mut sigil = false;
        if v[TQ_SIGIL_STRING] && lx.lookahead() == '~' {
            sigil = true;
            lx.next();
            match lx.lookahead() {
                's' | 'S' | 'b' | 'B' => lx.next(),
                '"' => {}
                _ => return false,
            }
        }
        // 三个或更多个引号开头，行内其余只能是空白；结尾引号个数必须相同
        for _ in 0..3 {
            if lx.lookahead() != '"' {
                return false;
            }
            lx.next();
        }
        let mut count: u32 = 3;
        while lx.lookahead() == '"' {
            count += 1;
            lx.next();
        }
        while lx.lookahead() != '\n' && is_whitespace(lx) {
            lx.next();
        }
        if lx.lookahead() != '\n' {
            return false;
        }
        lx.next();
        loop {
            if lx.lookahead() == '\n' {
                lx.next();
                while lx.lookahead() != '\n' && is_whitespace(lx) {
                    lx.next();
                }
                let mut check = count;
                while check > 0 && lx.lookahead() == '"' {
                    lx.next();
                    check -= 1;
                }
                if check == 0 {
                    lx.mark_end();
                    lx.set_result(if sigil { TQ_SIGIL_STRING } else { TQ_STRING } as u16);
                    return true;
                }
            } else if lx.eof() {
                return false;
            } else {
                lx.next();
            }
        }
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
