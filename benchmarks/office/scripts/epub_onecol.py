#!/usr/bin/env python3
"""核对 EPUB 里单栏表格（每行至多一格有字、内部没有嵌套表格）的文字：直接从 EPUB 的 XHTML 取每一格，
看 Markdown 输出里是否都在、先后顺序是否一致（归一规则同 score.py）。含 MathML 的格另计，不比对。
用法：epub_onecol.py <集> <输出目录>（在评测工作目录里运行）"""
import glob, html, os, re, sys, zipfile
S, OUT = sys.argv[1], sys.argv[2]
sys.argv = [sys.argv[0], "", ""]
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "score.py")).read().split("STRUCT = [")[0])
for f in sorted(glob.glob(f"{S}/epub/*.epub")):
    z = zipfile.ZipFile(f); md = norm(open(os.path.join(OUT, os.path.basename(f) + ".md")).read())
    tot = miss = disorder = math = 0
    for n in z.namelist():
        if not re.search(r"\.x?html?$", n): continue
        x = z.read(n).decode("utf-8", "replace")
        for t in re.findall(r"<table\b.*?</table>", x, re.S | re.I):
            if re.search(r"<table\b", t[6:], re.I): continue
            raw = [re.findall(r"<t[dh]\b.*?</t[dh]>", r, re.S | re.I) for r in re.findall(r"<tr\b.*?</tr>", t, re.S | re.I)]
            cells = [[(norm(html.unescape(re.sub(r"<[^>]+>", " ", c))), "math" in c) for c in r] for r in raw]
            if not cells or max(len([c for c, _ in r if c]) for r in cells) != 1: continue
            pos = 0
            for r in cells:
                for c, m in r:
                    if not c: continue
                    if m: math += 1; continue
                    tot += 1; p = md.find(c[:60], pos)
                    if p >= 0: pos = p
                    elif c[:60] in md: disorder += 1; print("乱序", f, c[:60])
                    else: miss += 1; print("丢", f, c[:60])
    if tot or math: print(f"{S}\t{os.path.basename(f)}\t文字格 {tot}\t丢 {miss}\t乱序 {disorder}\t含公式的格（另计）{math}")
