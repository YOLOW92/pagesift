//! 语言标记到内嵌语法表的映射，以及各语言的节点名规则。
//!
//! 表是编译进二进制的 zlib 压缩数据，第一次用到某种语言时才解压并缓存；没用到的语言不占内存。
//! 节点名以各官方语法的 `src/node-types.json` 为准。

use std::sync::OnceLock;

use crate::table::Language;

/// 能提取大纲的语言（`jsx` 用 JavaScript 语法，`php` 用带 HTML 文本的 PHP 语法）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    C,
    Cpp,
    CSharp,
    Go,
    Java,
    JavaScript,
    TypeScript,
    Tsx,
    Php,
    Python,
    Rust,
    Lua,
}

const COUNT: usize = 12;

impl Lang {
    /// `pagesift-office` 的语言标记转语言，不支持的返回 `None`。
    pub fn from_tag(tag: &str) -> Option<Lang> {
        Some(match tag {
            "c" => Lang::C,
            "cpp" => Lang::Cpp,
            "csharp" => Lang::CSharp,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "javascript" | "jsx" => Lang::JavaScript,
            "typescript" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "php" => Lang::Php,
            "python" => Lang::Python,
            "rust" => Lang::Rust,
            "lua" => Lang::Lua,
            _ => return None,
        })
    }

    fn index(self) -> usize {
        self as usize
    }

    fn compressed(self) -> &'static [u8] {
        match self {
            Lang::C => include_bytes!("../tables/c.pslg.z"),
            Lang::Cpp => include_bytes!("../tables/cpp.pslg.z"),
            Lang::CSharp => include_bytes!("../tables/csharp.pslg.z"),
            Lang::Go => include_bytes!("../tables/go.pslg.z"),
            Lang::Java => include_bytes!("../tables/java.pslg.z"),
            Lang::JavaScript => include_bytes!("../tables/javascript.pslg.z"),
            Lang::TypeScript => include_bytes!("../tables/typescript.pslg.z"),
            Lang::Tsx => include_bytes!("../tables/tsx.pslg.z"),
            Lang::Php => include_bytes!("../tables/php.pslg.z"),
            Lang::Python => include_bytes!("../tables/python.pslg.z"),
            Lang::Rust => include_bytes!("../tables/rust.pslg.z"),
            Lang::Lua => include_bytes!("../tables/lua.pslg.z"),
        }
    }

    /// 解压并缓存语法表；表损坏（不应发生）返回 `None`。
    pub fn table(self) -> Option<&'static Language> {
        static CACHE: [OnceLock<Option<Language>>; COUNT] = [const { OnceLock::new() }; COUNT];
        CACHE[self.index()]
            .get_or_init(|| {
                // 最大的表（C++）解压后约 6 MB，上限留到 32 MB
                let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                    self.compressed(),
                    32 << 20,
                )
                .ok()?;
                Language::from_bytes(&raw).ok()
            })
            .as_ref()
    }

    /// 本语言是否把 C 系声明（`declaration`、`type_definition`）当成可能带类型定义的包裹。
    pub(crate) fn is_c_family(self) -> bool {
        matches!(self, Lang::C | Lang::Cpp)
    }
}

use crate::extract::SymbolKind;

/// 符号的容器性质：类型容器里的函数算方法，作用域容器（命名空间、模块）里不算。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cont {
    No,
    Type,
    Scope,
}

/// 需要专门处理的节点。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sp {
    GoType,
    GoImport,
    JsVar,
    CDecl,
}

#[derive(Clone, Copy)]
pub(crate) enum Rule {
    Sym(SymbolKind, &'static str, Cont),
    /// 包裹层（export、decorated 等）：向下找声明，分段时算作一个整体。
    Wrap,
    /// 预处理条件等透明层：向下找声明，分段时展开成它的子节点。
    Flat,
    Import,
    Special(Sp),
    Skip,
}

use Cont::{No, Scope, Type as Ty};
use Rule::{Import, Skip, Sym, Wrap};
use SymbolKind as K;

/// 紧贴在符号前、归入该符号分段的节点类型（注释和 Rust 的属性）。
pub(crate) fn is_leading(kind: &str) -> bool {
    matches!(
        kind,
        "comment" | "line_comment" | "block_comment" | "attribute_item"
    )
}

pub(crate) fn rule(lang: Lang, kind: &str) -> Rule {
    match lang {
        Lang::Python => match kind {
            "function_definition" => Sym(K::Function, "function", No),
            "class_definition" => Sym(K::Class, "class", Ty),
            "decorated_definition" => Wrap,
            "import_statement" | "import_from_statement" | "future_import_statement" => Import,
            _ => Skip,
        },
        Lang::Rust => match kind {
            "function_item" | "function_signature_item" => Sym(K::Function, "function", No),
            "struct_item" => Sym(K::Struct, "struct", No),
            "union_item" => Sym(K::Struct, "union", No),
            "enum_item" => Sym(K::Enum, "enum", No),
            "trait_item" => Sym(K::Trait, "trait", Ty),
            "impl_item" => Sym(K::Module, "impl", Ty),
            "mod_item" => Sym(K::Module, "mod", Scope),
            "type_item" => Sym(K::Type, "type", No),
            "use_declaration" => Import,
            _ => Skip,
        },
        Lang::Go => match kind {
            "function_declaration" => Sym(K::Function, "function", No),
            "method_declaration" => Sym(K::Method, "method", No),
            "type_declaration" => Rule::Special(Sp::GoType),
            "import_declaration" => Rule::Special(Sp::GoImport),
            _ => Skip,
        },
        Lang::Java => match kind {
            "class_declaration" => Sym(K::Class, "class", Ty),
            "record_declaration" => Sym(K::Class, "record", Ty),
            "interface_declaration" => Sym(K::Interface, "interface", Ty),
            "annotation_type_declaration" => Sym(K::Interface, "interface", Ty),
            "enum_declaration" => Sym(K::Enum, "enum", Ty),
            "method_declaration" => Sym(K::Method, "method", No),
            "constructor_declaration" => Sym(K::Method, "constructor", No),
            "enum_body_declarations" => Wrap,
            "import_declaration" => Import,
            _ => Skip,
        },
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => match kind {
            "function_declaration" | "generator_function_declaration" => {
                Sym(K::Function, "function", No)
            }
            "class_declaration" | "abstract_class_declaration" => Sym(K::Class, "class", Ty),
            "method_definition" => Sym(K::Method, "method", No),
            "interface_declaration" => Sym(K::Interface, "interface", No),
            "type_alias_declaration" => Sym(K::Type, "type", No),
            "enum_declaration" => Sym(K::Enum, "enum", No),
            "internal_module" | "module" => Sym(K::Module, "namespace", Scope),
            "export_statement" | "ambient_declaration" => Wrap,
            "lexical_declaration" | "variable_declaration" => Rule::Special(Sp::JsVar),
            "import_statement" => Import,
            _ => Skip,
        },
        Lang::C | Lang::Cpp => match kind {
            "function_definition" => Sym(K::Function, "function", No),
            "struct_specifier" => Sym(K::Struct, "struct", Ty),
            "class_specifier" => Sym(K::Class, "class", Ty),
            "enum_specifier" => Sym(K::Enum, "enum", No),
            "namespace_definition" => Sym(K::Module, "namespace", Scope),
            "template_declaration" => Wrap,
            "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "linkage_specification"
            | "declaration_list" => Rule::Flat,
            "declaration" | "type_definition" => Rule::Special(Sp::CDecl),
            "preproc_include" => Import,
            _ => Skip,
        },
        Lang::CSharp => match kind {
            "class_declaration" => Sym(K::Class, "class", Ty),
            "record_declaration" => Sym(K::Class, "record", Ty),
            "struct_declaration" => Sym(K::Struct, "struct", Ty),
            "interface_declaration" => Sym(K::Interface, "interface", Ty),
            "enum_declaration" => Sym(K::Enum, "enum", No),
            "namespace_declaration" => Sym(K::Module, "namespace", Scope),
            "file_scoped_namespace_declaration" => Sym(K::Module, "namespace", No),
            "method_declaration" => Sym(K::Method, "method", No),
            "constructor_declaration" => Sym(K::Method, "constructor", No),
            "using_directive" => Import,
            _ => Skip,
        },
        Lang::Php => match kind {
            "function_definition" => Sym(K::Function, "function", No),
            "class_declaration" => Sym(K::Class, "class", Ty),
            "interface_declaration" => Sym(K::Interface, "interface", Ty),
            "trait_declaration" => Sym(K::Trait, "trait", Ty),
            "enum_declaration" => Sym(K::Enum, "enum", Ty),
            "method_declaration" => Sym(K::Method, "method", No),
            "namespace_definition" => Sym(K::Module, "namespace", Scope),
            "namespace_use_declaration" => Import,
            _ => Skip,
        },
        Lang::Lua => match kind {
            "function_declaration" => Sym(K::Function, "function", No),
            _ => Skip,
        },
    }
}
