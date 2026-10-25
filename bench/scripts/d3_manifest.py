#!/usr/bin/env python3
"""D3 清单：把 olmOCR-bench 的 PDF 与 7 类检查项整理成统一 manifest。"""
import json, collections
from pathlib import Path

SRC = Path("/home/yolow92/pagesift/bench/data/olmocr_1403/bench_data")
OUT = Path("/home/yolow92/pagesift/bench/data/showdown/d3")
OUT.mkdir(parents=True, exist_ok=True)

# PDF 清单（相对路径，容器内前缀 /data/olmocr_1403/bench_data/pdfs/）
pdfs = sorted(p.relative_to(SRC / "pdfs").as_posix() for p in (SRC / "pdfs").rglob("*.pdf"))
with (OUT / "pdfs.jsonl").open("w", encoding="utf-8") as f:
    for i, rel in enumerate(pdfs):
        f.write(json.dumps({"id": f"pdf-{i:05d}", "rel": rel,
                            "pdf": f"/data/olmocr_1403/bench_data/pdfs/{rel}"}) + "\n")

# 检查项：原样复制，交给官方 tests.py 判定
tests = 0
for jp in sorted(SRC.glob("*.jsonl")):
    for line in jp.open(encoding="utf-8"):
        if line.strip():
            tests += 1
print(json.dumps({"pdfs": len(pdfs), "tests": tests}, indent=2))
