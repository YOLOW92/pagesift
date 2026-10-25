#!/usr/bin/env python3
"""Crawl Maze 对账：把爬到的 URL 与 test-cases 里的 .found 目标比对。"""
import json, sys
from pathlib import Path
from urllib.parse import urlparse

exp = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
idx = Path(sys.argv[2])
out = Path(sys.argv[3])

seen = set()
if idx.exists():
    for line in idx.open(encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        u = r.get("url") or r.get("request_url") or ""
        if u:
            p = urlparse(u)
            seen.add(p.path)
            seen.add(u)

expected = exp["expected"]
hit = [u for u in expected if u in seen]
miss = [u for u in expected if u not in seen]
# 爬取过程中被当作页面抓到的 .found（说明导航到了目标）
found_pages = sorted(x for x in seen if x.endswith(".found"))
res = {
    "expected_count": len(expected),
    "discovered_count": len(hit),
    "recall": len(hit) / len(expected) if expected else 0.0,
    "crawled_urls": len(seen),
    "found_pages_visited": len(found_pages),
    "missing": sorted(miss),
}
out.write_text(json.dumps(res, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps({k: v for k, v in res.items() if k != "missing"}, ensure_ascii=False, indent=2))
print("\nmissing (前 15):")
for m in sorted(miss)[:15]:
    print(" ", m)
