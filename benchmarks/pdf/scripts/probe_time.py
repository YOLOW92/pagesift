import json, sys, time
from pathlib import Path
ROOT = Path("/home/yolow92/pagesift")
sys.path.insert(0, str(ROOT/"benchmarks/pdf/vendor/olmocr"))
from olmocr.bench.tests import load_tests
B = ROOT/"benchmarks/data/olmocr_1403/bench_data"
for name in ("headers_footers","arxiv_math","table_tests"):
    ts = load_tests(str(B/f"{name}.jsonl"))[:5]
    md = "# x\n\ntest content 123"
    t0=time.perf_counter()
    for t in ts:
        try: t.run(md)
        except Exception: pass
    dt=(time.perf_counter()-t0)/len(ts)
    print(f"{name:<18} {dt*1000:8.1f} ms/项  类型={ts[0].type}")
