#!/usr/bin/env python3
"""调试用：列出某个输出没保住的表格、单元格、正文样句（规则同 score.py）。用法：misses.py <truth 目录> <输出目录> [类别]"""
import json, os, sys, glob
sys.argv_backup = sys.argv; sys.argv = [sys.argv[0], sys.argv[1], sys.argv[2]]
src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "score.py")).read().split("STRUCT = [")[0]
exec(src)
want = sys.argv_backup[3] if len(sys.argv_backup) > 3 else None
for f in sorted(glob.glob(os.path.join(sys.argv[1], "*.json"))):
    t = json.load(open(f)); p = os.path.join(sys.argv[2], t["file"] + ".md")
    md = open(p).read() if os.path.exists(p) else ""
    heads, tables, items, alts = parse(md); full = norm(md)
    allcells = {c for rows, _, _ in tables for r in rows for c in r}
    if want in (None, "tables"):
        for tb in t["tables"]:
            want_c = [norm(c) for c in tb["cells"]]
            fits = lambda r, c: c == tb['cols'] and abs(r - tb['rows']) <= max(1, tb['rows'] // 10)
            ok = any((fits(len(rows), n) or fits(*raw)) and all(c in {x for r in rows for x in r} for c in want_c) for rows, n, raw in tables)
            if not ok:
                cand = [(len(rows), n, raw) for rows, n, raw in tables if any(c in {x for r in rows for x in r} for c in want_c)]
                print(t["file"], "表", (tb["rows"], tb["cols"]), "缺格", [c for c in want_c if c not in allcells], "输出里含样本的表尺寸", cand[:3])
    if want in (None, "text"):
        for x in t["text"]:
            if norm(x)[:60] not in full: print(t["file"], "样句", x[:60])
    if want in (None, "headings"):
        hs = {h for _, h in heads}
        for l, h in t["headings"]:
            if norm(h) not in hs: print(t["file"], "标题", l, h)
