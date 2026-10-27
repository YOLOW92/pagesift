#!/usr/bin/env python3
"""汇总 E 的原始数据（越低越好）：HTML 提取看每轮新/旧耗时比的中位数，CPU 看每轮新/旧比值的中位数。"""
import collections, os, re, statistics as st
E = os.path.join(os.path.dirname(os.path.abspath(__file__)), "raw")
print("== PDF"); print(open(f"{E}/pdf-summary.txt").read().strip())
rows = [l.rstrip("\n").split("\t") for l in open(f"{E}/pdf.tsv")][1:]
worst = max(rows, key=lambda x: float(x[5]) / float(x[1]))
print(f"最慢一份（对 0.73）：{worst[0]} {float(worst[5])/float(worst[1]):.2f}；内存最差 {max(float(x[6])/float(x[2]) for x in rows):.2f}")

print("== HTML 提取（extract_mt，页/秒越高越好；下面是耗时比，越低越好）")
d = collections.defaultdict(dict)
for l in open(f"{E}/extract.txt"):
    m = re.match(r"(r\d+) (old|new) t=(\d+) .*?([\d.]+) pages/s", l) or re.match(r"(r\d+) (old|new) t=(\d+) .*?([\d.]+)", l)
    if m:
        d[(m[1], m[3])][m[2]] = float(m[4])
for t in ("1", "16"):
    ratios = [v["old"] / v["new"] for (r, tt), v in d.items() if tt == t and len(v) == 2]
    if ratios:
        print(f"{t} 线程：耗时 新/旧 中位数 {st.median(ratios):.3f}  范围 {min(ratios):.3f}–{max(ratios):.3f}  n={len(ratios)}")

print("== 每请求 CPU（µs，越低越好）")
c = collections.defaultdict(dict)
for l in open(f"{E}/cpu.txt"):
    m = re.match(r"(r\d+) (old|new) (serve|mcp) .*cpu_us_per_req=([\d.]+)", l)
    if m:
        c[(m[1], m[3])][m[2]] = float(m[4])
for kind in ("serve", "mcp"):
    pairs = [v for (r, k), v in c.items() if k == kind and len(v) == 2]
    if pairs:
        ratios = [v["new"] / v["old"] for v in pairs]
        print(f"{kind}：旧 {st.median(v['old'] for v in pairs):.0f}  新 {st.median(v['new'] for v in pairs):.0f}  "
              f"每轮新/旧中位数 {100*(st.median(ratios)-1):+.1f}%  范围 {100*(min(ratios)-1):+.1f}%..{100*(max(ratios)-1):+.1f}%  n={len(pairs)}")
