//! Gleam 外部扫描器，移植自 tree-sitter-gleam 的 `src/scanner.c`（Apache-2.0）。
//! 无状态：序列化为空。

use super::{ScanLexer, Scanner};

const QUOTED_CONTENT: usize = 0;
const DOC_COMMENT_CONTENT: usize = 1;

#[derive(Default)]
pub struct Gleam;

impl Scanner for Gleam {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if v[QUOTED_CONTENT] {
            let mut has_content = false;
            loop {
                match lx.lookahead() {
                    '"' | '\\' => break,
                    '\0' => return false,
                    _ => {}
                }
                has_content = true;
                lx.next();
            }
            lx.set_result(QUOTED_CONTENT as u16);
            return has_content;
        }
        if v[DOC_COMMENT_CONTENT] {
            lx.set_result(DOC_COMMENT_CONTENT as u16);
            loop {
                if lx.eof() {
                    return true;
                }
                if lx.lookahead() == '\n' {
                    // 文档注释带上换行，Markdown 注入需要
                    lx.next();
                    return true;
                }
                lx.next();
            }
        }
        false
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
