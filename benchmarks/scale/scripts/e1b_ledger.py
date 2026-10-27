#!/usr/bin/env python3
"""E1-B 台账：把爬取产物折算成"发现 → 允许 → 请求 → 解析 → 唯一内容 → 输出"六段。"""
import hashlib, json, re, sys
from pathlib import Path
from urllib.parse import urlparse

root = Path(sys.argv[1]); out = Path(sys.argv[2])
hosts = ["h1", "h2", "h3", "h4"]
res = {}
for h in hosts:
    d = root / h
    idx = d / "index.jsonl"
    rows = []
    if idx.exists():
        for line in idx.open(encoding="utf-8"):
            if line.strip():
                try:
                    rows.append(json.loads(line))
                except Exception:
                    pass
    urls = [r.get("url", "") for r in rows]
    paths = [urlparse(u).path for u in urls]
    hashes = set()
    empty = 0
    for r in rows:
        p = d / r.get("file", "")
        if not p.exists():
            continue
        t = p.read_text(encoding="utf-8", errors="replace")
        if not t.strip():
            empty += 1
        hashes.add(hashlib.sha256(t.encode()).hexdigest())
    # 违规核查
    private_hits = [p for p in paths if p.startswith("/private/")]
    param_variants = [u for u in urls if "?ref=" in u]
    log = (root / f"crawl_{h}.log")
    err = 0
    if log.exists():
        m = re.search(r"crawled (\d+) pages, (\d+) errors", log.read_text(encoding="utf-8", errors="replace"))
        if m:
            err = int(m.group(2))
    res[h] = {
        "requested_pages": len(rows),
        "unique_url": len(set(urls)),
        "unique_content": len(hashes),
        "empty_output": empty,
        "robots_violations_private": len(private_hits),
        "param_variant_urls": len(param_variants),
        "errors": err,
    }
tot = {k: sum(res[h][k] for h in hosts) for k in res["h1"]}
summary = {"per_host": res, "total": tot}
out.write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps(summary, ensure_ascii=False, indent=2))
