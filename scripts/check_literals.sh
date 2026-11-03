#!/usr/bin/env bash
# 检查运行时代码里新写的数字字面量有没有标明来历。
#
#   scripts/check_literals.sh                # 工作区相对 HEAD 的改动（含暂存和未跟踪的新文件）
#   scripts/check_literals.sh <rev>          # 相对 <rev> 的改动，例如一个内部编号开始时的提交
#   scripts/check_literals.sh --all [路径…]  # 不看改动，列出指定文件（默认全部运行时代码）里所有未标注的字面量
#
# 每个字面量所在行、所在语句（跨多行时）的任一行，或紧挨在语句上面的连续注释里，要有下面的标记之一：
#   [协议]  协议、格式、物理单位等外部给定的常数，写明出处（RFC、规范章节）
#   [策略]  用户策略的默认值，写明推导依据
#   [δ]     全局风险水平
#   [推导]  公式里的系数，写明公式和出处
#   [待迁移] 0.81 已有的值，只是换了位置（迁移完成时去掉）
# 0、½、1、2 以及 1000/1024 的幂、60/3600/86400 这类单位换算不用标；元组下标（x.0）、数组下标（a[3]）、
# 属性 #[...] 里的数字不算。
# 只查运行时 crate 的 src/：跳过 tests/、examples/、benches/、*_tests.rs、#[cfg(test)] 模块、
# pagesift-fuzz、pagesift-testkit 和 build.rs。有未标注的字面量时退出码为 1。
set -euo pipefail
cd "$(dirname "$0")/.."

python3 -I - "$@" <<'PY'
import re
import subprocess
from collections import Counter
import sys
from pathlib import Path

TAGS = ("[协议]", "[策略]", "[δ]", "[推导]", "[待迁移]")
EXEMPT = {0, 0.5, 1, 2, 60, 3600, 86400} | {1000 ** k for k in range(1, 5)} | {1024 ** k for k in range(1, 5)}
# pagesift-rustls 是上游 rustls 的源码（见它的 FORK.md），不按本仓库的规则改
SKIP_CRATES = ("crates/pagesift-fuzz/", "crates/pagesift-testkit/", "crates/pagesift-rustls/")


def runtime_file(path: str) -> bool:
    if not (path.startswith("crates/") and path.endswith(".rs") and "/src/" in path):
        return False
    if path.startswith(SKIP_CRATES) or path.endswith("/build.rs"):
        return False
    name = path.rsplit("/", 1)[-1]
    if name.endswith("_tests.rs") or name == "tests.rs" or name.startswith("test_"):
        return False
    return not any(f"/{d}/" in path for d in ("tests", "examples", "benches"))


NUM = re.compile(
    r"0[xX][0-9a-fA-F_]+|0[oO][0-7_]+|0[bB][01_]+"
    r"|[0-9][0-9_]*(?:\.[0-9][0-9_]*)?(?:[eE][+-]?[0-9_]+)?"
)
IDENT = re.compile(r"[^\W\d]\w*")  # Rust 标识符可以是 Unicode（XID）


def lex(src: str):
    """把源码拆成（行号，数字字面量，前一个有效字符）和每行的注释文本。

    字符串、字符、注释里的数字不算；原始字符串 r#"…"# 和嵌套块注释都处理。"""
    nums, comments, code = [], {}, []
    i, line, n = 0, 1, len(src)
    prev = ""

    def note(text: str, start_line: int):
        for k, part in enumerate(text.split("\n")):
            comments.setdefault(start_line + k, []).append(part)

    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            code.append(c)
            i += 1
            continue
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            note(src[i:j], line)
            i = j
            continue
        if src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            note(src[i:j], line)
            line += src.count("\n", i, j)
            code.append("\n" * src.count("\n", i, j))
            i = j
            continue
        m = re.match(r'b?r(#*)"', src[i:])
        if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            end = '"' + m.group(1)
            j = src.find(end, i + m.end())
            j = n if j < 0 else j + len(end)
            line += src.count("\n", i, j)
            code.append(" " + "\n" * src.count("\n", i, j))
            prev, i = '"', j
            continue
        if c == '"' or (c == "b" and src.startswith('b"', i)):
            j = i + (2 if c == "b" else 1)
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            j += 1
            line += src.count("\n", i, j)
            code.append(" " + "\n" * src.count("\n", i, j))
            prev, i = '"', j
            continue
        if c == "'":
            # 字符字面量 'a'、'\n'、'\u{..}'；否则是生命周期 'a
            m = re.match(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'", src[i:])
            if m:
                code.append(" ")
                prev, i = "'", i + m.end()
                continue
        if c.isalpha() or c == "_":
            m = IDENT.match(src, i)
            code.append(m.group(0))
            prev, i = "a", m.end()
            continue
        if c.isdigit():
            m = NUM.match(src, i)
            j = m.end()
            s = IDENT.match(src, j)  # 类型后缀 u8 / f64 / usize
            text = m.group(0).rstrip("_")
            if s:
                j = s.end()
            nxt = src[j:j + 1]
            nums.append((line, text, prev if not (prev == "[" and nxt == "]") else "[]"))
            code.append("0")
            prev, i = "0", j
            continue
        if not c.isspace():
            prev = c
        code.append(c)
        i += 1
    return nums, comments, "".join(code)


def skipped_lines(code: str) -> set:
    """#[cfg(test)] 模块和属性 #[...] 所在的行。"""
    out = set()
    lines_at = [0]
    for ch in code:
        lines_at.append(lines_at[-1] + (ch == "\n"))

    def span(a: int, b: int):
        out.update(range(lines_at[a] + 1, lines_at[b] + 2))

    for m in re.finditer(r"#\s*!?\s*\[", code):
        depth, j = 1, m.end()
        while j < len(code) and depth:
            depth += {"[": 1, "]": -1}.get(code[j], 0)
            j += 1
        attr = code[m.start():j]
        if re.search(r"cfg\s*\(\s*test\s*\)", attr):
            k = code.find("{", j)
            semi = code.find(";", j)
            if k >= 0 and (semi < 0 or k < semi):
                depth, e = 1, k + 1
                while e < len(code) and depth:
                    depth += {"{": 1, "}": -1}.get(code[e], 0)
                    e += 1
                span(m.start(), e - 1)
                continue
        # 只标属性本身；属性和条目同一行时（#[x(3)] fn f()）整行都算属性，数字极少
        span(m.start(), j - 1)
    return out


def value(text: str):
    t = text.replace("_", "")
    try:
        if t[:2].lower() in ("0x", "0o", "0b"):
            return int(t, 0)
        f = float(t)
        return int(f) if f.is_integer() else f
    except ValueError:
        return None


def tagged(line: int, comments: dict, src_lines: list) -> bool:
    has = lambda k: any(t in c for c in comments.get(k, []) for t in TAGS)
    # 先退到所在语句的第一行：上一行没有以 ; { } 结尾、也不是空行或注释，就还是同一条语句
    k = line
    while True:
        if has(k):
            return True
        if k <= 1:
            break
        prev = src_lines[k - 2].strip()
        if not prev or prev.startswith("//") or prev.endswith((";", "{", "}")):
            break
        k -= 1
    k -= 1
    while k >= 1:
        stripped = src_lines[k - 1].strip()
        if not (stripped.startswith("//") or stripped.startswith("/*") or stripped.startswith("*")):
            break
        if any(t in stripped for t in TAGS):
            return True
        k -= 1
    return False


def check(path: str, wanted):
    src = Path(path).read_text(encoding="utf-8")
    nums, comments, code = lex(src)
    skip = skipped_lines(code)
    src_lines = src.split("\n")
    bad = []
    for line, text, prev in nums:
        if wanted is not None and line not in wanted:
            continue
        if line in skip or prev in (".", "[]"):
            continue
        v = value(text)
        if v in EXEMPT:
            continue
        if not tagged(line, comments, src_lines):
            bad.append((line, text))
    return bad


def git(*args) -> str:
    return subprocess.run(("git",) + args, check=True, capture_output=True, text=True).stdout


def changed(base: str) -> dict:
    """相对 base 新写的行。原样搬移的行（删掉的原文在改动里别处原样出现）不算新写，
    所以纯搬移拆分文件不会报。"""
    added, removed, cur, new_no = [], Counter(), None, 0
    for l in git("diff", "-U0", "--no-color", base, "--", "crates").splitlines():
        if l.startswith("+++ "):
            cur = l[6:] if l.startswith("+++ b/") else None
        elif l.startswith("--- "):
            continue
        elif l.startswith("@@"):
            new_no = int(re.search(r"\+(\d+)", l).group(1))
        elif l.startswith("-"):
            removed[l[1:].strip()] += 1
        elif l.startswith("+") and cur:
            added.append((cur, new_no, l[1:].strip()))
            new_no += 1
    for f in git("ls-files", "--others", "--exclude-standard", "--", "crates").splitlines():
        if f.endswith(".rs"):
            for k, text in enumerate(Path(f).read_text(encoding="utf-8").split("\n"), 1):
                added.append((f, k, text.strip()))
    out = {}
    for path, line, text in added:
        if removed[text] > 0:
            removed[text] -= 1
            continue
        out.setdefault(path, set()).add(line)
    return out


args = sys.argv[1:]
if args[:1] == ["--all"]:
    paths = args[1:] or git("ls-files", "--", "crates").splitlines()
    targets = {p: None for p in paths if runtime_file(p)}
else:
    targets = {p: l for p, l in changed(args[0] if args else "HEAD").items() if runtime_file(p)}

total = 0
for path in sorted(targets):
    if not Path(path).exists():
        continue
    for line, text in check(path, targets[path]):
        total += 1
        print(f"{path}:{line}: {text}")
if total:
    print(f"\n{total} 个数字字面量没有标注 {' / '.join(TAGS)}", file=sys.stderr)
    sys.exit(1)
print(f"check_literals: {len(targets)} 个文件，没有未标注的字面量", file=sys.stderr)
PY
