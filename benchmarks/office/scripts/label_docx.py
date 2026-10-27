#!/usr/bin/env python3
"""docx 的标题和列表真值（替换 pandoc 的候选）。规则（人工定，写在 REVIEW.md）：
- 标题：段落或样式继承链上有 outlineLvl（0–8；默认段落样式上的不算），或样式名是内置 Heading N / Title，
  或自定义样式名里有 Heading/Subheading/Title（模板里人眼看就是标题的样式；Subtitle、TOC、Caption 除外）。级别按 outlineLvl+1、
  Heading N 的 N；只能靠名字判断的按名字里的数字，没有数字记 0（只比文字不比级别）。
- 列表：带编号属性（段落或样式链上的 numPr，且 numId 不为 0）且不是标题的段落。
- 取样：标题最多 30 个、列表最多 20 个，在全文里均匀取。
用法：label_docx.py <truth 目录> <docx...>"""
import json, os, re, sys
from docx import Document
from docx.oxml.ns import qn
def chain(style):
    while style is not None:
        yield style; style = style.base_style
def outline(p):
    # 默认段落样式（通常是 Normal）上的大纲级别不算：有的模板给 Normal 设了 outlineLvl 0
    styles = [s for s in chain(p.style) if s.element.get(qn("w:default")) not in ("1", "true")]
    for el in [p._p.pPr] + [s.element.pPr for s in styles]:
        if el is None: continue
        o = el.find(qn("w:outlineLvl"))
        if o is not None and o.get(qn("w:val")) and int(o.get(qn("w:val"))) < 9: return int(o.get(qn("w:val"))) + 1
    return None
def numbered(p):
    for el in [p._p.pPr] + [s.element.pPr for s in chain(p.style)]:
        if el is None: continue
        n = el.find(qn("w:numPr"))
        if n is not None:
            nid = n.find(qn("w:numId"))
            return nid is not None and nid.get(qn("w:val")) != "0"
    return False
def heading(p):
    lvl = outline(p)
    if lvl: return lvl
    for s in chain(p.style):
        m = re.fullmatch(r"(?i)heading (\d)", s.name or "")
        if m: return int(m.group(1))
        if (s.name or "").lower() == "title": return 1
    name = p.style.name or ""
    if re.search(r"(?i)heading|subheading|title", name) and not re.search(r"(?i)toc|caption|table|subtitle", name):
        m = re.search(r"\d", name); return int(m.group(0)) if m else 0
    return None
def spread(xs, n): return xs if len(xs) <= n else [xs[i * len(xs) // n] for i in range(n)]
out = sys.argv[1]
for path in sys.argv[2:]:
    d = Document(path); hs = []; ls = []
    for p in d.paragraphs:
        t = re.sub(r"\s+", " ", p.text).strip()
        if not t: continue
        lvl = heading(p)
        if lvl is not None: hs.append([lvl, t])
        elif numbered(p): ls.append(t[:80])
    f = os.path.join(out, os.path.basename(path) + ".json"); j = json.load(open(f))
    j["headings"] = spread(hs, 30); j["lists"] = spread(ls, 20); j["order"] = [h[1] for h in spread(hs, 8)]
    json.dump(j, open(f, "w"), ensure_ascii=False, indent=1)
    print(os.path.basename(path), len(hs), "标题", len(ls), "列表项")
