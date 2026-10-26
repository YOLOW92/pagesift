#!/usr/bin/env python3
"""汇总耗时与峰值内存（都越低越好）。
耗时：office_bench / time_markitdown.py 的 TSV（文件名、中位数毫秒、输出字节）；按格式加总，再给两边合计之比
（小文件上 pagesift 只要微秒级，逐文件比值会被分母放大，所以不用逐文件比值的平均）。
内存：mem.sh 的 TSV（文件名、pagesift KB、MarkItDown KB）；按格式取中位数，第一行 empty.csv 是进程基础内存。
用法：summarize_perf.py <pagesift 耗时> <MarkItDown 耗时> <内存>"""
import statistics, sys, collections
def load(p):
    d = {}
    for l in open(p):
        f = l.rstrip("\n").split("\t")
        if len(f) >= 2 and f[1] not in ("", "NA"): d[f[0]] = float(f[1])
    return d
ext = lambda n: n.rsplit(".", 1)[-1].lower()
a, b = load(sys.argv[1]), load(sys.argv[2])
by = collections.defaultdict(list)
for n in sorted(set(a) & set(b)): by[ext(n)].append(n)
print("| 格式 | 文件数 | pagesift 合计 ms ↓ | MarkItDown 合计 ms ↓ | MarkItDown ÷ pagesift（合计）|")
print("| --- | --- | --- | --- | --- |")
alln = []
for e in ("docx", "xlsx", "pptx", "epub", "csv"):
    ns = by.get(e, []); alln += ns
    if not ns: continue
    sa, sb = sum(a[n] for n in ns), sum(b[n] for n in ns)
    print(f"| {e} | {len(ns)} | {sa:.1f} | {sb:.1f} | {sb / sa:.0f}× |")
sa, sb = sum(a[n] for n in alln), sum(b[n] for n in alln)
print(f"| 合计 | {len(alln)} | {sa:.1f} | {sb:.1f} | {sb / sa:.0f}× |")
print(f"\n只有一边有结果的文件：{sorted(set(a) ^ set(b))}")
rows = [l.rstrip("\n").split("\t") for l in open(sys.argv[3])]
base = rows[0]; mem = collections.defaultdict(list)
for r in rows[1:]:
    if len(r) == 3 and r[1] and r[2]: mem[ext(r[0])].append((int(r[1]) / 1024, int(r[2]) / 1024))
print(f"\n进程基础内存（1 行的 CSV）：pagesift {int(base[1]) / 1024:.1f} MB，MarkItDown {int(base[2]) / 1024:.1f} MB\n")
print("| 格式 | 文件数 | pagesift 峰值中位数 MB ↓ | MarkItDown 峰值中位数 MB ↓ | pagesift 最大 MB | MarkItDown 最大 MB |")
print("| --- | --- | --- | --- | --- | --- |")
allm = []
for e in ("docx", "xlsx", "pptx", "epub", "csv"):
    v = mem.get(e, []); allm += v
    if v: print(f"| {e} | {len(v)} | {statistics.median(x for x, _ in v):.1f} | {statistics.median(y for _, y in v):.1f} | {max(x for x, _ in v):.1f} | {max(y for _, y in v):.1f} |")
print(f"| 合计 | {len(allm)} | {statistics.median(x for x, _ in allm):.1f} | {statistics.median(y for _, y in allm):.1f} | {max(x for x, _ in allm):.1f} | {max(y for _, y in allm):.1f} |")
