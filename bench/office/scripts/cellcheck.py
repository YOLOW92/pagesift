#!/usr/bin/env python3
"""自检：LibreOffice（en-US）显示的每个非空单元格，在输出的表格单元格里有没有（不管位置）。
用法：cellcheck.py <lo html 目录> <输出目录>"""
import os, sys, re, collections
sys.argv_b = sys.argv; sys.argv = sys.argv[:1] + ["x", "y"]
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "score.py")).read().split("STRUCT = [")[0])
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "label_xlsx.py")).read().split("def spread")[0])
lo, out = sys.argv_b[1], sys.argv_b[2]
tot = ok = 0; miss = collections.Counter()
for f in sorted(os.listdir(lo)):
    if not f.endswith(".html"): continue
    p = P(); p.feed(open(os.path.join(lo, f), encoding="utf-8", errors="replace").read())
    grids = [g for _, g in p.sheets] or p.loose
    md = open(os.path.join(out, f[:-5] + ".xlsx.md"), encoding="utf-8").read()
    _, tables, _, _ = parse(md); have = collections.Counter(c for rows, _, _ in tables for r in rows for c in r)
    for g in grids:
        for v in g.values():
            if not v: continue
            tot += 1
            if have[norm(v)] > 0: ok += 1
            else: miss[(f[:25], v[:40])] += 1
print(f"单元格一致 {ok}/{tot} = {ok/max(tot,1):.4f}")
for (f, v), n in miss.most_common(25): print(n, f, repr(v))
