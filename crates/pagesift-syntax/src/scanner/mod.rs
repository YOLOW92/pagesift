//! 外部扫描器：官方语法里手写的 `scanner.c` 用 Rust 重写（trait 形式，语义与 `TSLexer` 一致）。
//!
//! 约定与官方相同：`scan` 按 `valid` 判断哪些外部记号此刻可用，成功时用 `set_result` 给出外部记号序号；
//! 状态通过 `serialize`/`deserialize` 随记号保存，解析器回退或分叉后会把状态恢复到最后一个外部记号之后。

use crate::lexer::Lexer;
use crate::table::EOF_CP;

pub mod cpp;
pub mod csharp;
pub mod js;
pub mod lua;
pub mod php;
pub mod python;
pub mod rust;
pub mod toml;
pub mod ts;

/// 序列化缓冲上限（与官方 `TREE_SITTER_SERIALIZATION_BUFFER_SIZE` 相同）。
pub const MAX_SERIALIZED: usize = 1024;

pub trait Scanner {
    fn scan(&mut self, lx: &mut ScanLexer, valid: &[bool]) -> bool;
    fn serialize(&self, out: &mut Vec<u8>);
    /// 空切片表示回到初始状态。
    fn deserialize(&mut self, data: &[u8]);
}

/// 按语法名创建扫描器，没有移植的语法返回 `None`。
pub fn create(name: &str) -> Option<Box<dyn Scanner>> {
    Some(match name {
        "python" => Box::new(python::Python::default()),
        "cpp" => Box::new(cpp::Cpp::default()),
        "lua" => Box::new(lua::Lua::default()),
        "toml" => Box::new(toml::Toml),
        "php" | "php_only" => Box::new(php::Php::default()),
        "csharp" | "c_sharp" => Box::new(csharp::CSharp::default()),
        "rust" => Box::new(rust::Rust::default()),
        "javascript" => Box::new(js::Js),
        "typescript" | "tsx" => Box::new(ts::Ts),
        _ => return None,
    })
}

/// 交给扫描器的词法器视图。输入不可信：操作次数有上限，超限后表现为输入结束，保证扫描一定终止。
pub struct ScanLexer<'l, 'a> {
    lx: &'l mut Lexer<'a>,
    ops: usize,
    limit: usize,
    pub result: u16,
}

impl<'l, 'a> ScanLexer<'l, 'a> {
    pub fn new(lx: &'l mut Lexer<'a>) -> Self {
        let limit = lx.input.len().saturating_sub(lx.pos.bytes as usize) + 4096;
        ScanLexer {
            lx,
            ops: 0,
            limit,
            result: 0,
        }
    }
    fn exhausted(&self) -> bool {
        self.ops > self.limit
    }
    /// 当前前瞻字符，输入结束返回 '\0'。
    pub fn lookahead(&self) -> char {
        if self.exhausted() {
            return '\0';
        }
        let c = self.lx.look();
        if c == EOF_CP {
            '\0'
        } else {
            char::from_u32(c).unwrap_or('\u{FFFD}')
        }
    }
    pub fn eof(&self) -> bool {
        self.exhausted() || self.lx.eof()
    }
    /// 前进一个字符；`skip` 为真时该字符算作记号前的空白。
    pub fn advance(&mut self, skip: bool) {
        self.ops += 1;
        if !self.exhausted() {
            self.lx.advance(skip);
        }
    }
    pub fn skip(&mut self) {
        self.advance(true);
    }
    pub fn next(&mut self) {
        self.advance(false);
    }
    pub fn mark_end(&mut self) {
        self.lx.mark_end();
    }
    pub fn set_result(&mut self, sym: u16) {
        self.result = sym;
    }
    /// 当前位置在行内的列号（按字符计）。
    pub fn column(&self) -> u32 {
        self.lx.column()
    }
}

/// 对应 C 的 `iswspace`（C.UTF-8 区域：不含不换行空格）。
pub fn is_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}') && c != '\u{85}'
}
/// 对应 C 的 `iswalpha`。
pub fn is_alpha(c: char) -> bool {
    c.is_alphabetic()
}
