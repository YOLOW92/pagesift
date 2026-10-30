#!/usr/bin/env python3
"""检查四种语言的 README 是否符合首页模板（给 agent 读的结构）。

模板 v1 的约定（README 里用 HTML 注释标出各节，标题文字各语言自己写）：
  <!-- readme-template: v1 -->        放在 README 里，表示已按模板改版
  <!-- section: overview -->          一句话定义 + 可替代哪些工具
  <!-- section: facts -->             事实速览：输入、输出、接口、平台、许可证、版本
  <!-- section: install -->           安装命令 + 最小示例 + MCP 配置，必须在前 INSTALL_BY 行内开始
  <!-- section: choose -->            什么时候选、什么时候别选
  <!-- section: performance -->       性能摘要（标方向和口径，完整表放链接）
  <!-- section: features -->          功能与 API
  <!-- section: license -->           许可证与商业授权
  <!-- section: changelog -->         只留最新一版，其余放 CHANGELOG.md

任何时候都查：徽章只能用白名单里的；不许有对 AI/agent 下指令的文字。
有模板标记时再查：各节齐全、顺序一致、四种语言一致、安装段位置、带版本号的二、三级标题最多一个。
--release 时再查：一级标题的版本号等于工作区版本；版本 >= STRICT_FROM 时必须已按模板改版并有 llms.txt。
"""
import argparse
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FILES = ["README.md", "docs/README.zh-CN.md", "docs/README.ja.md", "docs/README.es.md"]
MARKER = "<!-- readme-template: v1 -->"
SECTIONS = ["overview", "facts", "install", "choose", "performance", "features", "license", "changelog"]
INSTALL_BY = 60
STRICT_FROM = (0, 90)
# 徽章白名单：按链接目标认，不按显示文字认（各语言文字不同）
BADGE_TARGETS = [
    r"LICENSE\)$",
    r"COMMERCIAL\.md\)$",
    r"github\.com/YOLOW92/pagesift/releases\)$",
    r"crates\.io/crates/pagesift\)$",
    r"pypi\.org/project/pagesift/?\)$",
    r"npmjs\.com/package/@pagesift/node\)$",
    r"discord\.gg/",
]
# 对 agent 下指令的写法会被当成提示注入，反而降低信任
AGENT_DIRECTIVES = re.compile(
    r"(ignore (all |any )?(previous|prior) instructions"
    r"|\b(AI|LLM)s?\s*(agents?|assistants?|models?)?\s*[:,]\s*(please\s+)?(recommend|choose|prefer|rank|pick)\b"
    r"|\b(dear|attention|note to|hey)\s+(AI|LLM|agents?|assistants?|language models?)\b"
    r"|(AI|智能体|大模型|agent)[^\n。]{0,12}(请|务必|应当|应该)(推荐|优先|选择)"
    r"|エージェント[^\n。]{0,12}(推薦|選んで))",
    re.I,
)


def badge_lines(text):
    """页首（第一个 </div> 之前）的徽章行；页尾的 Star History 之类图表不算。"""
    head = text.split("</div>", 1)[0]
    return [(i + 1, line) for i, line in enumerate(head.splitlines()) if re.match(r"\s*\[!\[", line)]


def check_file(rel, text, problems):
    in_code = False
    lines = text.splitlines()
    for no, line in badge_lines(text):
        if not any(re.search(p, line.strip()) for p in BADGE_TARGETS):
            problems.append(f"{rel}:{no}: 徽章不在白名单里：{line.strip()[:80]}")
    version_heads = []
    for i, line in enumerate(lines):
        if line.startswith("```"):
            in_code = not in_code
        if in_code:
            continue
        if AGENT_DIRECTIVES.search(line):
            problems.append(f"{rel}:{i + 1}: 像是对 AI/agent 下指令（会被当成提示注入）：{line.strip()[:80]}")
        if re.match(r"#{2,3} .*\b\d+\.\d+(\.\d+)?\b", line):
            version_heads.append(i + 1)
    if MARKER in text and len(version_heads) > 1:
        problems.append(f"{rel}: 带版本号的二、三级标题有 {len(version_heads)} 个（行 {version_heads[:5]}…），"
                        "首页只留最新一版，其余放 CHANGELOG.md")


def sections(text):
    return [(text[:m.start()].count("\n") + 1, m.group(1))
            for m in re.finditer(r"<!-- section: ([a-z]+) -->", text)]


def check_template(rel, text, problems):
    found = sections(text)
    names = [n for _, n in found]
    if names != SECTIONS:
        problems.append(f"{rel}: 节标记应为 {SECTIONS}，实际 {names}")
    install = [no for no, n in found if n == "install"]
    if install and install[0] > INSTALL_BY:
        problems.append(f"{rel}:{install[0]}: 安装段要在前 {INSTALL_BY} 行内开始")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--release", action="store_true", help="发布前的完整检查")
    a = ap.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    problems, warnings = [], []
    texts = {rel: (ROOT / rel).read_text(encoding="utf-8") for rel in FILES}
    templated = [rel for rel, t in texts.items() if MARKER in t]
    for rel, text in texts.items():
        check_file(rel, text, problems)
        if MARKER in text:
            check_template(rel, text, problems)
    if templated and len(templated) != len(FILES):
        problems.append(f"只有 {templated} 按模板改版了，四种语言要一起改")
    if not templated:
        warnings.append(f"README 还没按模板 v1 改版（没有 {MARKER}），只查了徽章和指令性文字")
    if a.release:
        strict = tuple(int(x) for x in version.split(".")[:2]) >= STRICT_FROM
        for rel, text in texts.items():
            m = re.search(r"^# .*?(\d+\.\d+\.\d+)", text, re.M)
            if not m or m.group(1) != version:
                problems.append(f"{rel}: 一级标题的版本号 {m.group(1) if m else '缺失'} 不等于工作区版本 {version}")
        if strict and not templated:
            problems.append(f"{version} >= {'.'.join(map(str, STRICT_FROM))}：README 必须已按模板 v1 改版")
        if strict and not (ROOT / "llms.txt").is_file():
            problems.append("缺少根目录 llms.txt")
    for w in warnings:
        print(f"check_readme: 提示：{w}", file=sys.stderr)
    if problems:
        print("check_readme: README 不符合首页模板：", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        sys.exit(1)
    print(f"README OK ({'模板 v1' if templated else '旧版'}, {version})")


if __name__ == "__main__":
    main()
