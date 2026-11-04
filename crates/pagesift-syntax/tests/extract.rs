//! 每种语言一个小样例：符号、导入，以及所有分段拼起来等于原文。
use pagesift_syntax::{SymbolKind as K, outline};

fn check(tag: &str, src: &str, want: &[(K, &str)], imports: usize) {
    let o = outline(tag, src).unwrap_or_else(|| panic!("{tag}: 没有大纲"));
    let got: Vec<(K, &str)> = o
        .symbols
        .iter()
        .map(|s| (s.kind, s.name.as_str()))
        .collect();
    assert_eq!(got, want, "{tag}");
    assert_eq!(o.imports.len(), imports, "{tag}: {:?}", o.imports);
    let joined: String = o.sections.iter().map(|s| &src[s.start..s.end]).collect();
    assert_eq!(joined, src, "{tag}: 分段拼起来不等于原文");
}

#[test]
fn python() {
    let src = "import os\nfrom a import b\n\n@deco\ndef f():\n    pass\n\nclass C:\n    def m(self):\n        pass\n";
    check(
        "python",
        src,
        &[(K::Function, "f"), (K::Class, "C"), (K::Method, "m")],
        2,
    );
}

#[test]
fn rust() {
    let src = "use std::fmt;\n\n/// 文档\n#[inline]\npub fn f() {}\n\nstruct S;\nenum E { A }\ntrait T { fn g(); }\nimpl S { fn h(&self) {} }\nmod m { fn k() {} }\n";
    let want = [
        (K::Function, "f"),
        (K::Struct, "S"),
        (K::Enum, "E"),
        (K::Trait, "T"),
        (K::Method, "g"),
        (K::Module, "S"),
        (K::Method, "h"),
        (K::Module, "m"),
        (K::Function, "k"),
    ];
    check("rust", src, &want, 1);
    let o = outline("rust", src).unwrap();
    assert!(
        src[o.sections[1].start..o.sections[1].end].starts_with("/// 文档\n#[inline]\npub fn f")
    );
}

#[test]
fn go() {
    let src = "package p\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\ntype A struct{}\ntype B interface{}\ntype C int\n\nfunc F() {}\nfunc (a A) M() {}\n";
    check(
        "go",
        src,
        &[
            (K::Struct, "A"),
            (K::Interface, "B"),
            (K::Type, "C"),
            (K::Function, "F"),
            (K::Method, "M"),
        ],
        2,
    );
}

#[test]
fn java() {
    let src = "import java.util.List;\n\npublic class A {\n  A() {}\n  void m() {}\n}\ninterface I { void n(); }\n";
    check(
        "java",
        src,
        &[
            (K::Class, "A"),
            (K::Method, "A"),
            (K::Method, "m"),
            (K::Interface, "I"),
            (K::Method, "n"),
        ],
        1,
    );
}

#[test]
fn javascript() {
    let src = "import a from 'a';\n\nfunction f() {}\nconst g = () => 1;\nexport class C { m() {} }\nexport function h() {}\nconst x = 1;\n";
    check(
        "javascript",
        src,
        &[
            (K::Function, "f"),
            (K::Function, "g"),
            (K::Class, "C"),
            (K::Method, "m"),
            (K::Function, "h"),
        ],
        1,
    );
}

#[test]
fn typescript() {
    let src = "import a from 'a';\ninterface I { x: number }\ntype T = string;\nenum E { A }\nexport abstract class C { m(): void {} }\nfunction f(): void {}\n";
    let want = [
        (K::Interface, "I"),
        (K::Type, "T"),
        (K::Enum, "E"),
        (K::Class, "C"),
        (K::Method, "m"),
        (K::Function, "f"),
    ];
    check("typescript", src, &want, 1);
    check(
        "tsx",
        "const C = () => <div/>;\nfunction f() { return <a/>; }\n",
        &[(K::Function, "C"), (K::Function, "f")],
        0,
    );
}

#[test]
fn c_family() {
    let src = "#include <stdio.h>\n\nstruct S { int a; };\nstruct F;\ntypedef struct { int b; } T;\nenum E { A };\nint *f(int x) { return 0; }\n";
    check(
        "c",
        src,
        &[
            (K::Struct, "S"),
            (K::Struct, "T"),
            (K::Enum, "E"),
            (K::Function, "f"),
        ],
        1,
    );
    let src = "#include <x>\nnamespace n { class A { void m() {} }; }\ntemplate <class T> T g(T x) { return x; }\nint A::h() { return 0; }\n";
    check(
        "cpp",
        src,
        &[
            (K::Module, "n"),
            (K::Class, "A"),
            (K::Method, "m"),
            (K::Function, "g"),
            (K::Function, "A::h"),
        ],
        1,
    );
}

#[test]
fn csharp() {
    let src = "using System;\nnamespace N {\n  class A { void M() {} }\n  interface I {}\n}\n";
    check(
        "csharp",
        src,
        &[
            (K::Module, "N"),
            (K::Class, "A"),
            (K::Method, "M"),
            (K::Interface, "I"),
        ],
        1,
    );
}

#[test]
fn php_and_lua() {
    let src = "<?php\nuse A\\B;\nfunction f() {}\nclass C { function m() {} }\n";
    check(
        "php",
        src,
        &[(K::Function, "f"), (K::Class, "C"), (K::Method, "m")],
        1,
    );
    check(
        "lua",
        "local x = 1\nfunction f() end\nlocal function g() end\n",
        &[(K::Function, "f"), (K::Function, "g")],
        0,
    );
}

#[test]
fn unsupported_or_symbol_free_is_none() {
    assert!(outline("cobol", "x").is_none());
    assert!(outline("python", "x = 1\n").is_none());
    assert!(outline("python", "").is_none());
}

/// 坏输入：随机截断和替换字节，不崩溃，能结束，大纲要么没有要么还能拼回原文。
#[test]
fn damaged_input_never_panics() {
    let samples = [
        (
            "python",
            "def f(x):\n    if x:\n        return [1, 2]\n\nclass C:\n    def m(self): pass\n",
        ),
        (
            "rust",
            "fn f<T>(x: T) -> T { let y = x; y }\nimpl S { fn h(&self) {} }\n",
        ),
        (
            "c",
            "#ifdef A\nint f(int x) { return x + 1; }\n#endif\nstruct S { int a; };\n",
        ),
        (
            "javascript",
            "class C { m() { return `a${1}b`; } }\nfunction f() { /* c */ }\n",
        ),
        (
            "php",
            "<?php class C { function m() { echo \"a{$b}\"; } }\n?>text<?php function f() {}\n",
        ),
        (
            "csharp",
            "namespace N { class A { string M() => $\"a{1}\"; } }\n",
        ),
        ("lua", "function f() return [[x]] end\n"),
    ];
    let mut seed = 0x9E37_79B9_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize
    };
    for (tag, src) in samples {
        for _ in 0..200 {
            let mut b = src.as_bytes().to_vec();
            b.truncate(next() % (b.len() + 1));
            for _ in 0..3 {
                if !b.is_empty() {
                    let at = next() % b.len();
                    b[at] = b"{}()<>\"'`$#\n ;x"[next() % 15];
                }
            }
            let text = String::from_utf8_lossy(&b).into_owned();
            if let Some(o) = outline(tag, &text) {
                let joined: String = o.sections.iter().map(|s| &text[s.start..s.end]).collect();
                assert_eq!(joined, text);
            }
        }
    }
}
