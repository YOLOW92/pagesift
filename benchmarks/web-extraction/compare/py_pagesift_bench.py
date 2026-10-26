#!/usr/bin/env python3
"""Measure the pagesift Python package on the bench inputs: per-page latency on one thread,
then throughput with a plain ThreadPoolExecutor (convert releases the GIL).
Also writes JSON lines for `pagesift bench --external` to confirm output matches the Rust core.

    python -I benchmarks/web-extraction/compare/py_pagesift_bench.py benchmarks/data/inputs/manifest.jsonl out.jsonl [threads]
"""

import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pagesift


def main(manifest: Path, out_path: Path, threads: int) -> None:
    rows = [json.loads(l) for l in manifest.open() if l.strip()]
    pages = [(r["id"], Path(r["path"]).read_text(encoding="utf-8"), r.get("base_url") or r["url"]) for r in rows]

    lat = []
    with out_path.open("w") as out:
        for pid, html, url in pages:
            t = time.perf_counter()
            doc = pagesift.convert(html, url=url)
            ms = (time.perf_counter() - t) * 1000
            lat.append(ms)
            out.write(json.dumps({"id": pid, "markdown": doc.markdown, "ms": round(ms, 3)}, ensure_ascii=False) + "\n")
    lat.sort()
    print(f"single thread: p50 {lat[len(lat)//2]:.2f} ms, p95 {lat[int(len(lat)*0.95)]:.2f} ms, total {sum(lat)/1000:.2f} s")

    by_size = sorted(pages, key=lambda p: -len(p[1]))
    best = float("inf")
    for _ in range(2):
        t = time.perf_counter()
        with ThreadPoolExecutor(threads) as pool:
            list(pool.map(lambda p: pagesift.convert(p[1], url=p[2]), by_size))
        best = min(best, time.perf_counter() - t)
    print(f"{threads} threads: {len(pages)/best:.0f} pages/s ({best*1000:.0f} ms wall)")


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3]) if len(sys.argv) > 3 else os.cpu_count())
