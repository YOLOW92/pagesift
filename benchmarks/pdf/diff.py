"""逐条比较两个工具在某个子集上的测试结果：python diff.py <子集> <工具A> <工具B>
先按 PDF 名的 sha1 奇偶分成调参半（tune）和留出半（hold），分别给出两个工具的通过率（越高越好）；
再列出每个 PDF 上 B 比 A 多通过的测试数（从多到少），用来找差距最大的文档。"""
import os, sys, collections, hashlib
from olmocr.bench.tests import load_tests

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
subset, a, b = sys.argv[1:4]
tests = load_tests(os.path.join(out, subset + ".jsonl"))

def md(tool, pdf):
    p = os.path.join(out, tool, pdf.replace(".pdf", "_pg1_repeat1.md"))
    try:
        return open(p, encoding="utf-8").read()
    except OSError:
        return ""

cache, gap, total = {}, collections.Counter(), collections.Counter()
half = collections.Counter()
def part(pdf):
    return "tune" if hashlib.sha1(pdf.encode()).digest()[0] % 2 == 0 else "hold"
for t in tests:
    if t.type == "baseline":
        continue
    ra = t.run(cache.setdefault((a, t.pdf), md(a, t.pdf)))[0]
    rb = t.run(cache.setdefault((b, t.pdf), md(b, t.pdf)))[0]
    total[t.pdf] += 1
    half[(part(t.pdf), "n")] += 1
    half[(part(t.pdf), a)] += ra
    half[(part(t.pdf), b)] += rb
    if rb and not ra:
        gap[t.pdf] += 1
    if ra and not rb:
        gap[t.pdf] -= 1
for h in ("tune", "hold"):
    n = max(half[(h, "n")], 1)
    print(f"{h}: {a} {100 * half[(h, a)] / n:.1f}%  {b} {100 * half[(h, b)] / n:.1f}%  ({n} tests)")
print("net", sum(gap.values()), "of", sum(total.values()))
for pdf, n in gap.most_common(25):
    print(n, total[pdf], pdf)
