//! PowerShell 外部扫描器，移植自 tree-sitter-powershell 的 `src/scanner.c`
//! （MIT，Copyright (c) Microsoft Corporation）。无状态：序列化为空。

use super::{ScanLexer, Scanner, is_space};

const STATEMENT_TERMINATOR: usize = 0;
const BARE_DASHES: usize = 1;

#[derive(Default)]
pub struct PowerShell;

fn scan_statement_terminator(lx: &mut ScanLexer, v: &[bool]) -> bool {
    if !v[STATEMENT_TERMINATOR] {
        return false;
    }
    lx.set_result(STATEMENT_TERMINATOR as u16);
    // 这个记号不含字符，是否存在全靠前瞻
    lx.mark_end();
    loop {
        match lx.lookahead() {
            '\0' | '}' | ';' | ')' | '\n' => return true,
            c if !is_space(c) => return false,
            _ => lx.skip(),
        }
    }
}

/// 单独的 `-` 或 `--` 参数（`git log -- path`、`tar -xf -`）：后面必须是空白或命令结尾。
fn scan_bare_dashes(lx: &mut ScanLexer, v: &[bool]) -> bool {
    if !v[BARE_DASHES] {
        return false;
    }
    while matches!(lx.lookahead(), ' ' | '\t') {
        lx.skip();
    }
    if lx.lookahead() != '-' {
        return false;
    }
    lx.next();
    if lx.lookahead() == '-' {
        lx.next();
    }
    lx.mark_end();
    match lx.lookahead() {
        '\0' | ' ' | '\t' | '\r' | '\n' | ';' | '|' | '&' | ')' | '}' => {
            lx.set_result(BARE_DASHES as u16);
            true
        }
        _ => false,
    }
}

impl Scanner for PowerShell {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        // 空的语句结束符要放最后：两者都有效时 `--` 优先
        if scan_bare_dashes(lx, v) {
            return true;
        }
        scan_statement_terminator(lx, v)
    }

    fn serialize(&self, _out: &mut Vec<u8>) {}

    fn deserialize(&mut self, _data: &[u8]) {}
}
