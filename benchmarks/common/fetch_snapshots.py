#!/usr/bin/env python3
"""Fetch raw HTML snapshots for the Firecrawl scrape-content-dataset-v1 URLs.

Snapshots make `pagesift bench` reproducible and offline. Fetching is polite:
robots.txt is honoured, one request at a time per host, and the User-Agent
identifies the benchmark. Only the Python standard library is used.

    python3 benchmarks/common/fetch_snapshots.py dataset/1-0-0.csv snapshots/
"""

import csv
import json
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.robotparser
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import urlsplit

UA = "pagesift-bench/0.0.1 (+https://github.com/YOLOW92/pagesift)"
TIMEOUT = 20
MAX_BYTES = 15 * 1024 * 1024
WORKERS = 16

host_locks = defaultdict(threading.Lock)
robots_cache = {}
robots_lock = threading.Lock()


def robots_allows(url):
    parts = urlsplit(url)
    origin = f"{parts.scheme}://{parts.netloc}"
    with robots_lock:
        rp = robots_cache.get(origin)
    if rp is None:
        rp = urllib.robotparser.RobotFileParser()
        try:
            req = urllib.request.Request(origin + "/robots.txt", headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
                rp.parse(r.read(512 * 1024).decode("utf-8", "replace").splitlines())
        except urllib.error.HTTPError as e:
            # RFC 9309: 4xx means no restrictions, 5xx means assume full disallow
            rp.parse([] if e.code < 500 else ["User-agent: *", "Disallow: /"])
        except Exception:
            rp.parse([])
        with robots_lock:
            robots_cache[origin] = rp
    return rp.can_fetch(UA, url)


def fetch(row, out):
    url = row["url"]
    meta = {"id": row["id"], "url": url}
    try:
        host = urlsplit(url).netloc.lower()
        with host_locks[host]:
            if not robots_allows(url):
                meta["error"] = "robots_disallowed"
                return meta
            req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "text/html,*/*;q=0.8"})
            start = time.monotonic()
            with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
                body = r.read(MAX_BYTES)
                meta.update(status=r.status, final_url=r.geturl(),
                            content_type=r.headers.get("Content-Type", ""))
            meta["fetch_ms"] = round((time.monotonic() - start) * 1000)
            time.sleep(1)
        (out / f"{row['id']}.html").write_bytes(body)
        meta["bytes"] = len(body)
    except urllib.error.HTTPError as e:
        meta.update(status=e.code, error=f"http_{e.code}")
    except Exception as e:
        meta["error"] = type(e).__name__ + ": " + str(e)[:160]
    return meta


def main():
    dataset, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    csv.field_size_limit(1 << 30)
    rows = list(csv.DictReader(dataset.open(encoding="utf-8")))
    done = {json.loads(l)["id"] for l in (out / "meta.jsonl").open()} if (out / "meta.jsonl").exists() else set()
    todo = [r for r in rows if r["id"] not in done]
    print(f"{len(todo)} to fetch ({len(done)} already done)", flush=True)
    with (out / "meta.jsonl").open("a") as log, ThreadPoolExecutor(WORKERS) as pool:
        for i, meta in enumerate(pool.map(lambda r: fetch(r, out), todo), 1):
            log.write(json.dumps(meta, ensure_ascii=False) + "\n")
            log.flush()
            if i % 50 == 0:
                print(f"  {i}/{len(todo)}", flush=True)


if __name__ == "__main__":
    main()
