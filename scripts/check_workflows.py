#!/usr/bin/env python3
"""发布前检查 .github/workflows/*.yml 的常见错误（只用标准库，不解析完整 YAML，按缩进读我们自己写的格式）。

查出：
  1. 空的 `permissions:`（后面没有内容也不是 `{}`）：GitHub 会拒绝整个工作流（1.1.0 踩过）；
  2. step 既没有 `uses` 也没有 `run`；
  3. `needs` 指向不存在的 job。
用法：check_workflows.py [目录或文件 ...]，默认 .github/workflows。有问题退出码为 1。
"""
import re
import sys
from pathlib import Path


def indent(line):
    return len(line) - len(line.lstrip(" "))


def clean(lines):
    """去掉注释行、空行和多行字符串的内容，保留 (行号, 缩进, 文本)。"""
    out, block_indent = [], None
    for n, raw in enumerate(lines, 1):
        line = raw.rstrip("\n")
        if block_indent is not None:
            if not line.strip() or indent(line) > block_indent:
                continue
            block_indent = None
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        out.append((n, indent(line), line.strip()))
        if re.search(r":\s*[|>][+-]?\s*(#.*)?$", line):
            block_indent = indent(line) + (2 if line.lstrip().startswith("- ") else 0)
    return out


KEY = re.compile(r"(?:- )?([\w-]+):")


def step_problems(items, path, i, ind):
    """items[i] 是 `steps:`；列表项的 "- " 与它同缩进或再缩进 2 格。"""
    errs = []
    j, dash = i + 1, None
    while j < len(items) and items[j][1] >= ind:
        if items[j][2].startswith("- ") and (dash is None or items[j][1] == dash):
            dash = items[j][1]
            k = j + 1
            while k < len(items) and items[k][1] > dash:
                k += 1
            keys = set()
            for m in range(j, k):
                match = KEY.match(items[m][2])
                if match and items[m][1] in (dash, dash + 2):
                    keys.add(match.group(1))
            if not keys & {"uses", "run"}:
                errs.append(f"{path}:{items[j][0]}: step 既没有 uses 也没有 run")
            j = k
        else:
            j += 1
    return errs


def check(path):
    errs = []
    items = clean(path.read_text(encoding="utf-8").splitlines(keepends=True))
    jobs, needs = {}, []
    in_jobs, cur_job = False, None
    for i, (n, ind, text) in enumerate(items):
        nxt = items[i + 1] if i + 1 < len(items) else None
        if re.fullmatch(r"permissions:\s*(#.*)?", text) and not (nxt and nxt[1] > ind):
            errs.append(f"{path}:{n}: 空的 permissions:（写 `{{}}` 表示无权限，或列出具体权限）")
        if text == "steps:":
            errs += step_problems(items, path, i, ind)
        if ind == 0:
            in_jobs = text == "jobs:"
            cur_job = None
            continue
        if in_jobs and ind == 2 and text.endswith(":"):
            cur_job = text[:-1].strip().strip("'\"")
            jobs[cur_job] = n
        if in_jobs and cur_job and ind == 4 and text.startswith("needs:"):
            rest = text[6:].strip()
            if rest:
                names = [x.strip().strip("'\"") for x in rest.strip("[]").split(",")]
            else:
                names, j = [], i + 1
                while j < len(items) and items[j][1] >= ind and items[j][2].startswith("- "):
                    names.append(items[j][2][2:].strip().strip("'\""))
                    j += 1
            needs += [(n, cur_job, x) for x in names if x]
    for n, job, target in needs:
        if target not in jobs:
            errs.append(f"{path}:{n}: job {job} 的 needs 指向不存在的 job：{target}")
    return errs


def main(argv):
    targets = [Path(a) for a in argv] or [Path(".github/workflows")]
    files = []
    for t in targets:
        files += sorted(t.glob("*.y*ml")) if t.is_dir() else [t]
    errs = [e for f in files for e in check(f)]
    for e in errs:
        print(e)
    print(f"checked {len(files)} workflow files, {len(errs)} problems")
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
