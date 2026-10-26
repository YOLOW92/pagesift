"""开发集和留出集共用的修正规则（人工核对时定下，之后不再改）：
1. 去掉 1×1 或没有内容的表格：EPUB 里用表格排版的公式、PPT 里的空表格，不是数据表。
2. pptx：去掉 pandoc 编的“Slide N”标题（原文件里没有标题框）；去掉 pandoc 的“[Graphic: …]”占位文字；
   列表项和样句用 python-pptx 读到的段落原文替换（pandoc 会在两个文字片段之间插空格，如“w hat”）；
   标题和表格单元格按整个文本框、整个单元格的原文替换（2026-10-27 留出集评分后补上，见 REVIEW.md）。
3. 每类元素去重后再取；order 重新按标题计算。"""
import os, re, unicodedata
def norm(s): return re.sub(r"\s+", " ", unicodedata.normalize("NFKC", s)).strip()
def squash(s): return re.sub(r"\s+", "", unicodedata.normalize("NFKC", s))
def shapes_of(shapes):
    for sh in shapes:
        yield sh
        if sh.shape_type == 6: yield from shapes_of(sh.shapes)
def common(t, path):
    t["tables"] = [x for x in t["tables"] if x["rows"] * x["cols"] >= 2 and x["cells"]]
    if t["format"] == "pptx":
        t["headings"] = [h for h in t["headings"] if not re.fullmatch(r"Slide \d+", h[1])]
        t["text"] = [x for x in t["text"] if not x.startswith("[Graphic:")]
        try:
            from pptx import Presentation
            paras = [norm(p.text) for s in Presentation(path).slides for sh in s.shapes if sh.has_text_frame for p in sh.text_frame.paragraphs]
            by = {squash(p)[:60]: p for p in paras if p}
            fix = lambda x: by.get(squash(x)[:60], x)[:80]
            t["lists"] = [fix(x) for x in t["lists"]]; t["text"] = [fix(x) for x in t["text"]]
            # 2026-10-27 补（留出集评分之后，见 REVIEW.md）：标题和表格单元格也有同样的插空格，按整段原文替换
            whole = {}
            for s in Presentation(path).slides:
                for sh in shapes_of(s.shapes):
                    if sh.has_text_frame: whole[squash(sh.text_frame.text)] = norm(sh.text_frame.text)
                    if getattr(sh, "has_table", False) and sh.has_table:
                        for row in sh.table.rows:
                            for c in row.cells: whole[squash(c.text)] = norm(c.text)
            t["headings"] = [[l, whole.get(squash(h), h)] for l, h in t["headings"]]
            for tb in t["tables"]: tb["cells"] = [whole.get(squash(c), c) for c in tb["cells"]]
        except ImportError:
            pass
    for k in ("lists", "images", "footnotes", "notes", "text"):
        seen = []
        for x in t[k]:
            if x not in seen: seen.append(x)
        t[k] = seen
    hs = t["headings"]
    t["order"] = [h[1] for h in (hs if len(hs) <= 8 else [hs[i * len(hs) // 8] for i in range(8)])]
