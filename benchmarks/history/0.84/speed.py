#!/usr/bin/env python3
"""对一个正在运行的 `pagesift serve` 测单请求延迟和并发吞吐（新旧二进制 A/B 用）。

  speed.py <serve 地址> <站点地址> <页数> <并发>

每页一次 `engine: http`、`formats: [markdown]`。并发为 1 时报延迟中位数和 p95（毫秒，越低越好），
否则报每秒页数（越高越好）。输出一行 JSON。
"""
import json
import statistics
import sys
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor

API, SITE, N, C = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])


def one(i):
    body = json.dumps({"url": f"{SITE}/p/{i}", "formats": ["markdown"], "engine": "http"}).encode()
    req = urllib.request.Request(f"{API}/v2/scrape", data=body, headers={"Content-Type": "application/json"})
    t = time.perf_counter()
    try:
        urllib.request.urlopen(req, timeout=120).read()
    except urllib.error.HTTPError as e:
        e.read()
    return (time.perf_counter() - t) * 1000


t0 = time.perf_counter()
with ThreadPoolExecutor(C) as pool:
    ms = sorted(pool.map(one, range(N)))
wall = time.perf_counter() - t0
print(json.dumps({"concurrency": C, "pages": N, "p50_ms": round(statistics.median(ms), 3),
                  "p95_ms": round(ms[int(0.95 * (N - 1))], 3), "pages_per_s": round(N / wall, 1)}))
