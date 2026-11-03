"""Common Crawl lookups with HTTP Range requests only (data.commoncrawl.org).

index.commoncrawl.org is not needed: `cluster.idx` is a sorted text file, so a binary search with
small Range reads finds the compressed CDX block that holds a SURT key; the block and the WARC
record are fetched with Range requests too. Each request is one small read; the caller paces them.
"""

import gzip
import io
import json
import ssl
import time
import urllib.parse
import urllib.request

BASE = "https://data.commoncrawl.org/"
UA = "pagesift-bench/0.93 (+https://github.com/YOLOW92/pagesift)"
CTX = ssl.create_default_context(cafile="/root/.ccr/ca-bundle.crt")
# Pause between requests to the same host (seconds): politeness, not a tuning knob.
PAUSE = 1.0


def _get(url, start=None, end=None, retries=4):
    headers = {"User-Agent": UA}
    if start is not None:
        headers["Range"] = f"bytes={start}-{end}"
    for attempt in range(retries):
        try:
            time.sleep(PAUSE)
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), context=CTX, timeout=120) as r:
                return r.read(), r.headers
        except Exception:
            if attempt == retries - 1:
                raise
            time.sleep(2 ** (attempt + 1))


def size(url):
    req = urllib.request.Request(url, method="HEAD", headers={"User-Agent": UA})
    with urllib.request.urlopen(req, context=CTX, timeout=60) as r:
        return int(r.headers["Content-Length"])


def surt(url):
    u = urllib.parse.urlsplit(url)
    host = u.hostname or ""
    if host.startswith("www."):
        host = host[4:]
    parts = host.split(".")[::-1]
    path = u.path or "/"
    q = ("?" + u.query) if u.query else ""
    return ",".join(parts) + ")" + path.lower() + q.lower()


def _line_at(idx_url, total, pos):
    """First complete line starting at or after byte `pos`."""
    chunk, _ = _get(idx_url, pos, min(total - 1, pos + 65535))
    if pos > 0:
        nl = chunk.find(b"\n")
        chunk = chunk[nl + 1:]
    line = chunk.split(b"\n", 1)[0]
    return line.decode("utf-8", "replace")


def find_block(crawl, key):
    """The cluster.idx line whose block may contain `key` (the last line with SURT <= key)."""
    idx_url = f"{BASE}cc-index/collections/{crawl}/indexes/cluster.idx"
    total = size(idx_url)
    lo, hi = 0, total
    best = None
    while hi - lo > 65536:
        mid = (lo + hi) // 2
        line = _line_at(idx_url, total, mid)
        k = line.split(" ", 1)[0]
        if k <= key:
            best = line
            lo = mid
        else:
            hi = mid
    # scan the final window
    chunk, _ = _get(idx_url, lo, min(total - 1, hi + 65535))
    for raw in chunk.split(b"\n")[1 if lo else 0:]:
        line = raw.decode("utf-8", "replace")
        if not line:
            continue
        if line.split(" ", 1)[0] <= key:
            best = line
        else:
            break
    return best


def lookup(crawl, url):
    """CDX records (dicts) for `url` in `crawl`, HTML 200 responses only."""
    key = surt(url)
    line = find_block(crawl, key)
    if not line:
        return []
    _, rest = line.split(" ", 1)
    _, cdx_file, offset, length, _ = rest.split("\t")
    data, _ = _get(f"{BASE}cc-index/collections/{crawl}/indexes/{cdx_file}", int(offset), int(offset) + int(length) - 1)
    out = []
    for raw in gzip.decompress(data).split(b"\n"):
        if not raw.startswith(key.encode() + b" "):
            continue
        rec = json.loads(raw.split(b" ", 2)[2])
        if rec.get("status") == "200" and "html" in rec.get("mime", ""):
            out.append(rec)
    return out


def fetch_record(rec):
    """Raw gzip member bytes of the WARC record."""
    start = int(rec["offset"])
    data, _ = _get(BASE + rec["filename"], start, start + int(rec["length"]) - 1)
    return data


def html_of(member):
    from warcio.archiveiterator import ArchiveIterator

    for r in ArchiveIterator(io.BytesIO(member)):
        if r.rec_type == "response":
            body = r.content_stream().read()
            return body.decode("utf-8", "replace")
    return None
