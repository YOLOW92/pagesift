import json, re, statistics, sys
from pathlib import Path
ROOT = Path("/home/yolow92/pagesift")
sys.path.insert(0, str(ROOT/"benchmarks/pdf/vendor/olmocr"))
from olmocr.bench.tests import load_tests
B = ROOT/"benchmarks/data/olmocr_1403/bench_data"
d = ROOT/"out/d3/md070"
idx = {}
for f in d.glob("**/*.md"):
    m = re.match(r"^(.*)_pg(\d+)_repeat\d+\.md$", f.relative_to(d).as_posix())
    if m: idx[(m.group(1), int(m.group(2)))] = f.read_text(encoding="utf-8", errors="replace")
res = {}
for jp in sorted(B.glob("*.jsonl")):
    r = []
    for t in load_tests(str(jp)):
        md = idx.get((Path(t.pdf).with_suffix("").as_posix(), t.page))
        try: ok, _ = t.run(md) if md is not None else (False, None)
        except Exception: ok = False
        r.append(1.0 if ok else 0.0)
    res[jp.stem] = {"n": len(r), "rate": statistics.fmean(r)}
    print(f"{jp.stem:<24}{len(r):>6}{res[jp.stem]['rate']:>9.4f}", flush=True)
(ROOT/"out/d3/perjsonl.json").write_text(json.dumps(res, indent=2))
