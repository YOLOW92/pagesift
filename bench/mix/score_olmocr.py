"""B1：用 olmOCR-bench 自己的测试逐条判分（在 olmocr-bench 目录下、用它的 Python 环境运行）。

    cd <olmocr-bench> && PYTHONPATH=. <py>/bin/python -I <本文件> <bench_data 目录> <候选目录> <结果.jsonl>

候选目录里是 <pdf 相对路径去掉 .pdf>_pg1_repeat1.md（与 benchmark.py 的约定相同）；没有输出的页，它的测试都记失败
（benchmark.py 对缺文件也是这样）。直接调官方测试的 run()，不加预筛。
和官方 benchmark.py 一样，给每个没有基线测试的 PDF 加一条 BaselineTest，类别记作 "baseline"；总分是包括 baseline
在内的各类通过率的平均（benchmark.py 的 Average Score）。每条测试写一行 {pdf, category, type, id, passed}。
"""
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, os.getcwd())
from olmocr.bench.tests import BaselineTest, load_tests

data, cand, out = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
tests = []
for jl in sorted(data.glob("*.jsonl")):
    tests += [(jl.stem, t) for t in load_tests(str(jl))]
for pdf in sorted({t.pdf for _, t in tests}):
    if not any(t.type == "baseline" and t.pdf == pdf for _, t in tests):
        tests.append(("baseline", BaselineTest(id=f"{pdf}_baseline", pdf=pdf, page=1, type="baseline")))
with open(out, "w", encoding="utf-8") as fo:
    for cat, t in tests:
        md = cand / (os.path.splitext(t.pdf)[0] + f"_pg{t.page}_repeat1.md")
        if not md.exists():
            passed, why = False, "missing"
        else:
            try:
                passed, why = t.run(md.read_text(encoding="utf-8", errors="replace"))
            except Exception as e:  # noqa: BLE001  与 benchmark.py 一样，测试本身出错记失败
                passed, why = False, f"error: {e}"
        fo.write(json.dumps({"pdf": t.pdf, "category": cat, "type": t.type, "id": t.id, "passed": bool(passed)}) + "\n")
