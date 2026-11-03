#!/usr/bin/env python3
"""对一个正在运行的 `pagesift serve` 逐页请求，写出去掉易变字段后的结果。

  snapshot.py <serve 地址> <站点地址> <页数> <out.jsonl>

每页 `engine: http` 请求全部格式；每 10 页再加一次 includeTags/excludeTags 和一次 `engine: auto`
（服务端的浏览器连不上，只走"要不要渲染"的判断）。去掉耗时、时间戳这类每次都会变的字段。
"""
import json
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

API, SITE, N, OUT = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
FORMATS = ["markdown", "html", "rawHtml", "links", "images"]


def jobs():
    yield ("feed", {"url": f"{SITE}/feed.xml", "formats": FORMATS, "engine": "http"})
    yield ("json", {"url": f"{SITE}/data.json", "formats": FORMATS, "engine": "http"})
    for i in range(N):
        url = f"{SITE}/p/{i}"
        yield (f"http-{i}", {"url": url, "formats": FORMATS, "engine": "http"})
        if i % 10 == 0:
            yield (f"tags-{i}", {"url": url, "formats": ["markdown"], "engine": "http",
                                 "includeTags": ["p", "h1", "h2"], "excludeTags": ["nav", ".ad"]})
            yield (f"auto-{i}", {"url": url, "formats": ["markdown"], "engine": "auto"})


def scrub(v):
    if isinstance(v, dict):
        return {k: scrub(x) for k, x in v.items()
                if k not in ("timings", "cachedAt", "scrapeId", "creditsUsed", "elapsedMs")}
    if isinstance(v, list):
        return [scrub(x) for x in v]
    return v


def one(job):
    key, body = job
    req = urllib.request.Request(f"{API}/v2/scrape", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            out = json.loads(r.read())
    except urllib.error.HTTPError as e:
        out = {"httpStatus": e.code, "body": json.loads(e.read() or b"null")}
    return key, scrub(out)


with ThreadPoolExecutor(16) as pool, open(OUT, "w", encoding="utf-8") as f:
    for key, out in pool.map(one, jobs()):
        f.write(json.dumps({"key": key, "out": out}, sort_keys=True, ensure_ascii=False) + "\n")
