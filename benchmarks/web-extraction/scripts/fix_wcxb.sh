#!/bin/bash
cd ~/pagesift/benchmarks/data/wcxb
B="https://huggingface.co/datasets/murrough-foley/web-content-extraction-benchmark/resolve/main"
# 用 ground-truth 的 id 列表推断应有 html
python3 - <<'PY'
import json, os, subprocess, time
from pathlib import Path
root = Path("/home/yolow92/pagesift/benchmarks/data/wcxb")
base = "https://huggingface.co/datasets/murrough-foley/web-content-extraction-benchmark/resolve/main"
for split in ("dev", "test"):
    gt = root / split / "ground-truth"
    html = root / split / "html"
    html.mkdir(parents=True, exist_ok=True)
    ids = [p.stem for p in gt.glob("*.json")]
    missing = [i for i in ids if not (html / f"{i}.html").exists()]
    print(split, "gt", len(ids), "missing html", len(missing), flush=True)
    for n, i in enumerate(missing):
        url = f"{base}/{split}/html/{i}.html"
        dst = html / f"{i}.html"
        for attempt in range(6):
            r = subprocess.run(["curl", "-sSL", "--fail", "--max-time", "60", "-o", str(dst), url],
                               capture_output=True)
            if r.returncode == 0 and dst.stat().st_size > 0:
                break
            time.sleep(2 + attempt * 3)
        else:
            print("FAILED", i, flush=True)
        if n % 50 == 0:
            print(f"  {split} {n}/{len(missing)}", flush=True)
PY
echo FIX_DONE
for s in dev test; do echo "$s: html=$(ls ~/pagesift/benchmarks/data/wcxb/$s/html | wc -l)"; done
