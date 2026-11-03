"""Check pagesift's WARC reader against warcio on a Common Crawl sample (real WARC/1.0 records from
two crawls, one gzip member per record; collected by benchmarks/history/0.93/fetch.py).

Counts the 2xx response records warcio sees and compares them with the pages `pagesift scrape
sample.warc.gz --format json` converts (same URLs, every page has Markdown).

    python -I check_cc_reader.py target/quick/pagesift benchmarks/data/0.93/cc/sample.warc.gz
"""

import json
import subprocess
import urllib.parse
import sys
import time

from warcio.archiveiterator import ArchiveIterator

binary, path = sys.argv[1], sys.argv[2]


def norm(u):
    # URL 解析会把空路径写成 `/`（WHATWG URL），比较前两边都这样规范化
    p = urllib.parse.urlsplit(u)
    return urllib.parse.urlunsplit((p.scheme, p.netloc, p.path or "/", p.query, ""))


expected = []
records = 0
with open(path, "rb") as f:
    for r in ArchiveIterator(f):
        records += 1
        if r.rec_type == "response" and r.http_headers and r.http_headers.get_statuscode().startswith("2"):
            uri = r.rec_headers.get_header("WARC-Target-URI")
            if uri not in expected:
                expected.append(uri)
t0 = time.perf_counter()
out = subprocess.run([binary, "scrape", path, "--format", "json"], capture_output=True, text=True)
elapsed = time.perf_counter() - t0
pages = [json.loads(l) for l in out.stdout.splitlines() if l.strip()]
got = [p["data"]["metadata"]["sourceURL"] for p in pages]
empty = [p["data"]["metadata"]["sourceURL"] for p in pages if not (p["data"].get("markdown") or "").strip()]
summary = {
    "records": records,
    "warcio_2xx_responses": len(expected),
    "pagesift_pages": len(got),
    "same_urls": sorted(map(norm, got)) == sorted(map(norm, expected)),
    "pages_without_markdown": len(empty),
    "exit_code": out.returncode,
    "seconds": round(elapsed, 3),
}
print(json.dumps(summary, indent=2))
sys.exit(0 if summary["same_urls"] and out.returncode == 0 else 1)
