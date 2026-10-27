#!/usr/bin/env python3
"""E1-C 分析：预期拒绝 vs 意外错误分开，核查 robots/SSRF/重定向环与资源回落。"""
import collections, json, re, sys
from pathlib import Path

root = Path(sys.argv[1]); out = Path(sys.argv[2])
EXPECTED = {"RATE_LIMITED", "SERVER_ERROR", "TIMEOUT", "NETWORK", "ROBOTS_DISALLOWED"}
hosts = sorted(p.name for p in root.iterdir() if p.is_dir() and (p / "index.jsonl").exists())
res = {"per_host": {}, "invariants": {}}
allcodes = collections.Counter()
for h in hosts:
    log = root / f"crawl_{h}.log"
    txt = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
    codes = collections.Counter(re.findall(r"\b([A-Z][A-Z_]{3,})\b", txt))
    codes.pop("URL", None); codes.pop("HTTP", None)
    allcodes.update(codes)
    m = re.search(r"crawled (\d+) pages, (\d+) errors", txt)
    pages = int(m.group(1)) if m else 0
    errs = int(m.group(2)) if m else 0
    idx = [json.loads(l) for l in (root / h / "index.jsonl").open(encoding="utf-8") if l.strip()]
    urls = [r.get("url", "") for r in idx]
    res["per_host"][h] = {
        "pages": pages, "errors": errs, "codes": dict(codes),
        "unique_url": len(set(urls)),
        "private_path_hits": sum(1 for u in urls if "/private/" in u),
        "goto_private_hits": sum(1 for u in urls if "go-private" in u),
        "redirect_loop_hits": sum(1 for u in urls if "/r/loop" in u),
    }
# 不变量
res["invariants"] = {
    "no_private_path_fetched": all(v["private_path_hits"] == 0 for v in res["per_host"].values()),
    "ssrf_blocked_or_not_followed": all(v["goto_private_hits"] <= 1 for v in res["per_host"].values()),
    "redirect_loop_bounded": all(v["redirect_loop_hits"] <= 2 for v in res["per_host"].values()),
    "codes_seen": dict(allcodes),
}
exp = sum(allcodes[k] for k in EXPECTED if k in allcodes)
other = {k: v for k, v in allcodes.items() if k not in EXPECTED}
res["expected_rejections"] = exp
res["unexpected_codes"] = other
out.write_text(json.dumps(res, ensure_ascii=False, indent=2), encoding="utf-8")
print(json.dumps(res, ensure_ascii=False, indent=2)[:2500])
