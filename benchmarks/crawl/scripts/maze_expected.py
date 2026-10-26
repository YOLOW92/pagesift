#!/usr/bin/env python3
"""从 security-crawl-maze 的 test-cases 提取所有应被发现的 .found URL。"""
import re, json, sys
from pathlib import Path

root = Path(sys.argv[1]); out = Path(sys.argv[2])
pat = re.compile(r"""['"](/test/[^'"\s>]+\.found)['"]""")
found = set()
per_file = {}
for f in sorted(root.rglob("*")):
    if not f.is_file() or f.suffix not in {".html", ".js", ".css", ".txt", ".xml", ".json"}:
        continue
    try:
        t = f.read_text(encoding="utf-8", errors="replace")
    except Exception:
        continue
    for m in pat.finditer(t):
        u = m.group(1)
        found.add(u)
        per_file.setdefault(str(f.relative_to(root)), set()).add(u)
data = {"expected": sorted(found), "count": len(found),
        "by_file": {k: sorted(v) for k, v in per_file.items()}}
out.write_text(json.dumps(data, indent=2), encoding="utf-8")
print(f"expected .found URLs: {len(found)} from {len(per_file)} files")
