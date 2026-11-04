//! Dockerfile 外部扫描器，移植自 tree-sitter-dockerfile 的 `src/scanner.c`（MIT，Copyright (c) 2021 Camden Cheek）。

use super::{MAX_SERIALIZED, ScanLexer, Scanner, is_space};

const MAX_HEREDOCS: usize = 10;
const DEL_SPACE: usize = 512;

const HEREDOC_MARKER: usize = 0;
const HEREDOC_LINE: usize = 1;
const HEREDOC_END: usize = 2;
const HEREDOC_NL: usize = 3;
const ERROR_SENTINEL: usize = 4;

#[derive(Default)]
pub struct Dockerfile {
    in_heredoc: bool,
    stripping: bool,
    /// 每项第一个字节是 `-`（缩进剥离）或空格，其后是分隔符。
    heredocs: Vec<Vec<u8>>,
}

fn skip_whitespace(lx: &mut ScanLexer) {
    while lx.lookahead() != '\0' && lx.lookahead() != '\n' && is_space(lx.lookahead()) {
        lx.skip();
    }
}

impl Dockerfile {
    fn scan_marker(&mut self, lx: &mut ScanLexer) -> bool {
        skip_whitespace(lx);
        if lx.lookahead() != '<' {
            return false;
        }
        lx.next();
        if lx.lookahead() != '<' {
            return false;
        }
        lx.next();
        let mut stripping = false;
        if lx.lookahead() == '-' {
            stripping = true;
            lx.next();
        }
        let mut quote = '\0';
        if matches!(lx.lookahead(), '"' | '\'') {
            quote = lx.lookahead();
            lx.next();
        }
        let mut delim: Vec<u8> = vec![0];
        // 记满之后继续前进但不再记录，保证标记本身解析正确
        let mut recording = true;
        while lx.lookahead() != '\0'
            && (if quote != '\0' {
                lx.lookahead() != quote
            } else {
                !is_space(lx.lookahead())
            })
        {
            if lx.lookahead() == '\\' {
                lx.next();
                if lx.lookahead() == '\0' {
                    return false;
                }
            }
            if recording {
                delim.push(lx.lookahead() as u32 as u8);
            }
            lx.next();
            if recording && delim.len() >= DEL_SPACE - 2 {
                recording = false;
            }
        }
        if quote != '\0' {
            if lx.lookahead() != quote {
                return false;
            }
            lx.next();
        }
        if !recording {
            lx.set_result(HEREDOC_MARKER as u16);
            return true;
        }
        delim[0] = if stripping { b'-' } else { b' ' };
        if self.heredocs.is_empty() {
            self.heredocs.push(delim);
            self.stripping = stripping;
        } else if self.heredocs.len() < MAX_HEREDOCS {
            self.heredocs.push(delim);
        }
        lx.set_result(HEREDOC_MARKER as u16);
        true
    }

    fn scan_content(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if self.heredocs.is_empty() {
            self.in_heredoc = false;
            return false;
        }
        self.in_heredoc = true;
        if self.stripping {
            skip_whitespace(lx);
        }
        if v[HEREDOC_END] {
            let mut i = 1;
            let first = &self.heredocs[0];
            while i < first.len()
                && lx.lookahead() != '\0'
                && first[i] < 0x80
                && lx.lookahead() == first[i] as char
            {
                lx.next();
                i += 1;
            }
            if i == first.len() {
                lx.set_result(HEREDOC_END as u16);
                self.heredocs.remove(0);
                match self.heredocs.first() {
                    Some(h) => self.stripping = h[0] == b'-',
                    None => self.in_heredoc = false,
                }
                return true;
            }
        }
        if !v[HEREDOC_LINE] {
            return false;
        }
        lx.set_result(HEREDOC_LINE as u16);
        loop {
            match lx.lookahead() {
                '\0' => {
                    if lx.eof() {
                        self.in_heredoc = false;
                        return true;
                    }
                    lx.next();
                }
                '\n' => return true,
                _ => lx.next(),
            }
        }
    }
}

impl Scanner for Dockerfile {
    fn scan(&mut self, lx: &mut ScanLexer, v: &[bool]) -> bool {
        if v[ERROR_SENTINEL] {
            return if self.in_heredoc {
                self.scan_content(lx, v)
            } else {
                self.scan_marker(lx)
            };
        }
        if v[HEREDOC_NL] && !self.heredocs.is_empty() && lx.lookahead() == '\n' {
            lx.set_result(HEREDOC_NL as u16);
            lx.next();
            return true;
        }
        if v[HEREDOC_MARKER] {
            return self.scan_marker(lx);
        }
        if v[HEREDOC_LINE] || v[HEREDOC_END] {
            return self.scan_content(lx, v);
        }
        false
    }

    fn serialize(&self, out: &mut Vec<u8>) {
        out.push(self.in_heredoc as u8);
        out.push(self.stripping as u8);
        for h in &self.heredocs {
            // 放不下的丢掉；还要给结尾的 0 留一个字节
            if out.len() + h.len() + 1 + 1 > MAX_SERIALIZED {
                break;
            }
            out.extend_from_slice(h);
            out.push(0);
        }
        out.push(0);
    }

    fn deserialize(&mut self, d: &[u8]) {
        self.heredocs.clear();
        if d.len() < 2 {
            self.in_heredoc = false;
            self.stripping = false;
            return;
        }
        self.in_heredoc = d[0] != 0;
        self.stripping = d[1] != 0;
        let mut p = 2;
        for _ in 0..MAX_HEREDOCS {
            let Some(len) = d.get(p..).and_then(|r| r.iter().position(|b| *b == 0)) else {
                break;
            };
            if len == 0 {
                break;
            }
            self.heredocs.push(d[p..p + len].to_vec());
            p += len + 1;
        }
    }
}
