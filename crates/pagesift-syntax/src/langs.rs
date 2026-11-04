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
    Bash,
    Ruby,
    Kotlin,
    Css,
    Html,
    ObjC,
    Dart,
    Julia,
    Nix,
    Dockerfile,
    Ocaml,
    Elixir,
    R,
    Svelte,
    Solidity,
    Groovy,
    Make,
    Scheme,
    CommonLisp,
    Pascal,
    Gleam,
    PowerShell,
    CMake,
}

const COUNT: usize = 35;

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
            "bash" | "sh" | "shell" | "zsh" => Lang::Bash,
            "ruby" => Lang::Ruby,
            "kotlin" => Lang::Kotlin,
            "css" => Lang::Css,
            "html" | "htm" => Lang::Html,
            "objc" | "objective-c" | "objectivec" => Lang::ObjC,
            "dart" => Lang::Dart,
            "julia" | "jl" => Lang::Julia,
            "nix" => Lang::Nix,
            "dockerfile" => Lang::Dockerfile,
            "ocaml" | "ml" => Lang::Ocaml,
            "elixir" | "ex" | "exs" => Lang::Elixir,
            "r" => Lang::R,
            "svelte" => Lang::Svelte,
            "solidity" | "sol" => Lang::Solidity,
            "groovy" | "gradle" => Lang::Groovy,
            "make" | "makefile" | "mk" => Lang::Make,
            "scheme" | "scm" => Lang::Scheme,
            "commonlisp" | "lisp" | "cl" => Lang::CommonLisp,
            "pascal" | "pas" | "delphi" => Lang::Pascal,
            "gleam" => Lang::Gleam,
            "powershell" | "ps1" | "pwsh" => Lang::PowerShell,
            "cmake" => Lang::CMake,
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
            Lang::Bash => include_bytes!("../tables/bash.pslg.z"),
            Lang::Ruby => include_bytes!("../tables/ruby.pslg.z"),
            Lang::Kotlin => include_bytes!("../tables/kotlin.pslg.z"),
            Lang::Css => include_bytes!("../tables/css.pslg.z"),
            Lang::Html => include_bytes!("../tables/html.pslg.z"),
            Lang::ObjC => include_bytes!("../tables/objc.pslg.z"),
            Lang::Dart => include_bytes!("../tables/dart.pslg.z"),
            Lang::Julia => include_bytes!("../tables/julia.pslg.z"),
            Lang::Nix => include_bytes!("../tables/nix.pslg.z"),
            Lang::Dockerfile => include_bytes!("../tables/dockerfile.pslg.z"),
            Lang::Ocaml => include_bytes!("../tables/ocaml.pslg.z"),
            Lang::Elixir => include_bytes!("../tables/elixir.pslg.z"),
            Lang::R => include_bytes!("../tables/r.pslg.z"),
            Lang::Svelte => include_bytes!("../tables/svelte.pslg.z"),
            Lang::Solidity => include_bytes!("../tables/solidity.pslg.z"),
            Lang::Groovy => include_bytes!("../tables/groovy.pslg.z"),
            Lang::Make => include_bytes!("../tables/make.pslg.z"),
            Lang::Scheme => include_bytes!("../tables/scheme.pslg.z"),
            Lang::CommonLisp => include_bytes!("../tables/commonlisp.pslg.z"),
            Lang::Pascal => include_bytes!("../tables/pascal.pslg.z"),
            Lang::Gleam => include_bytes!("../tables/gleam.pslg.z"),
            Lang::PowerShell => include_bytes!("../tables/powershell.pslg.z"),
            Lang::CMake => include_bytes!("../tables/cmake.pslg.z"),
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
    ExCall,
    RAssign,
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
        // 标记语言没有“定义”可提：能解析，大纲为空
        Lang::Css | Lang::Html => Skip,
        Lang::Kotlin => match kind {
            "function_declaration" => Sym(K::Function, "function", No),
            "class_declaration" => Sym(K::Class, "class", Ty),
            "object_declaration" => Sym(K::Class, "object", Ty),
            "type_alias" => Sym(K::Type, "typealias", No),
            "import_list" => Rule::Flat,
            "import_header" => Import,
            _ => Skip,
        },
        Lang::Ruby => match kind {
            "method" | "singleton_method" => Sym(K::Function, "method", No),
            "class" => Sym(K::Class, "class", Ty),
            "module" => Sym(K::Module, "module", Ty),
            _ => Skip,
        },
        Lang::Bash => match kind {
            "function_definition" => Sym(K::Function, "function", No),
            _ => Skip,
        },
        Lang::ObjC => match kind {
            "class_interface" | "class_implementation" => Sym(K::Class, "class", Ty),
            "protocol_declaration" => Sym(K::Interface, "protocol", Ty),
            "method_definition" => Sym(K::Function, "method", No),
            "function_definition" => Sym(K::Function, "function", No),
            "preproc_include" | "module_import" => Import,
            _ => Skip,
        },
        Lang::Dart => match kind {
            "class_definition" | "mixin_application_class" => Sym(K::Class, "class", Ty),
            "mixin_declaration" => Sym(K::Trait, "mixin", Ty),
            "extension_declaration" | "extension_type_declaration" => {
                Sym(K::Module, "extension", Ty)
            }
            "enum_declaration" => Sym(K::Enum, "enum", Ty),
            "type_alias" => Sym(K::Type, "typedef", No),
            "function_signature" => Sym(K::Function, "function", No),
            "constructor_signature"
            | "factory_constructor_signature"
            | "constant_constructor_signature" => Sym(K::Method, "constructor", No),
            "method_signature" | "declaration" => Wrap,
            "import_or_export" => Import,
            _ => Skip,
        },
        Lang::Julia => match kind {
            "function_definition" | "macro_definition" => Sym(K::Function, "function", No),
            "struct_definition" => Sym(K::Struct, "struct", No),
            "abstract_definition" | "primitive_definition" => Sym(K::Type, "type", No),
            "module_definition" => Sym(K::Module, "module", Scope),
            "import_statement" | "using_statement" => Import,
            _ => Skip,
        },
        Lang::Nix => Skip,
        Lang::Dockerfile => Skip,
        Lang::Ocaml => match kind {
            "value_definition" | "type_definition" | "module_definition" | "class_definition" => {
                Wrap
            }
            "let_binding" => Sym(K::Function, "let", No),
            "type_binding" => Sym(K::Type, "type", No),
            "class_binding" => Sym(K::Class, "class", Ty),
            "module_binding" => Sym(K::Module, "module", Scope),
            "module_type_definition" => Sym(K::Interface, "module type", No),
            "open_module" => Import,
            _ => Skip,
        },
        Lang::Elixir => match kind {
            "call" => Rule::Special(Sp::ExCall),
            _ => Skip,
        },
        Lang::R => match kind {
            "binary_operator" => Rule::Special(Sp::RAssign),
            _ => Skip,
        },
        Lang::Svelte => Skip,
        Lang::Solidity => match kind {
            "contract_declaration" => Sym(K::Class, "contract", Ty),
            "interface_declaration" => Sym(K::Interface, "interface", Ty),
            "library_declaration" => Sym(K::Module, "library", Ty),
            "struct_declaration" => Sym(K::Struct, "struct", No),
            "enum_declaration" => Sym(K::Enum, "enum", No),
            "function_definition" => Sym(K::Function, "function", No),
            "modifier_definition" => Sym(K::Function, "modifier", No),
            "import_directive" => Import,
            _ => Skip,
        },
        Lang::Groovy => match kind {
            "class_definition" => Sym(K::Class, "class", Ty),
            "function_definition" | "function_declaration" => Sym(K::Function, "function", No),
            "groovy_import" => Import,
            _ => Skip,
        },
        Lang::Make => Skip,
        Lang::Scheme => Skip,
        Lang::CommonLisp => match kind {
            "list_lit" => Wrap,
            "defun" => Sym(K::Function, "defun", No),
            _ => Skip,
        },
        Lang::Pascal => match kind {
            "unit" | "program" => Sym(K::Module, "unit", Scope),
            "defProc" => Wrap,
            "declProc" => Sym(K::Function, "procedure", No),
            "declUses" => Import,
            _ => Skip,
        },
        Lang::Gleam => match kind {
            "function" | "external_function" => Sym(K::Function, "fn", No),
            "type_definition" => Sym(K::Type, "type", No),
            "type_alias" => Sym(K::Type, "type", No),
            "import" => Import,
            _ => Skip,
        },
        Lang::PowerShell => match kind {
            "statement_list" => Wrap,
            "function_statement" => Sym(K::Function, "function", No),
            "class_statement" => Sym(K::Class, "class", Ty),
            "enum_statement" => Sym(K::Enum, "enum", No),
            "class_method_definition" => Sym(K::Function, "method", No),
            _ => Skip,
        },
        Lang::CMake => match kind {
            "function_def" => Sym(K::Function, "function", No),
            "macro_def" => Sym(K::Function, "macro", No),
            _ => Skip,
        },
    }
}
