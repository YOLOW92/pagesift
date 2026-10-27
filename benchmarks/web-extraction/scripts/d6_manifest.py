#!/usr/bin/env python3
"""D6 清单：按 chardet 目录名 "编码-语言" 组织测试用例。"""
import json, collections
from pathlib import Path

ROOT = Path("/home/yolow92/pagesift/benchmarks/data/fresh/data/chardet-test-data")
OUT = Path("/home/yolow92/pagesift/benchmarks/data/showdown/d6")
OUT.mkdir(parents=True, exist_ok=True)

rows = []
for p in sorted(ROOT.iterdir()):
    if not p.is_dir() or p.name.startswith(".") or p.name in ("scripts", "None-None"):
        continue
    parts = p.name.rsplit("-", 1)
    if len(parts) != 2:
        continue
    enc, lang = parts
    for f in sorted(p.iterdir()):
        if f.is_file() and not f.name.startswith(".") and f.suffix != ".md":
            rows.append({"id": str(f), "path": str(f),
                         "path_container": "/data/fresh/data/chardet-test-data/" + f.relative_to(ROOT).as_posix(),
                         "expected": enc, "lang": lang})

with (OUT / "manifest.jsonl").open("w", encoding="utf-8") as f:
    for r in rows:
        f.write(json.dumps(r, ensure_ascii=False) + "\n")
print(json.dumps({"files": len(rows),
                  "encodings": len({r["expected"] for r in rows}),
                  "langs": len({r["lang"] for r in rows})}, indent=2))
