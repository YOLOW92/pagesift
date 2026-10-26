#!/usr/bin/env python3
"""Build a fresh, random page corpus for the release benchmarks.

Samples N sites uniformly at random from a Tranco ranking (ranks LO..HI), fetches each home page
politely (robots.txt honoured, one request at a time per host, an identifying User-Agent), then
up to K same-site content pages linked from it. Pages that are not HTML, are tiny, or look like
access barriers are dropped. Output:

    <out>/pages/<id>.html      raw HTML
    <out>/manifest.jsonl       {id, url, final_url, site_rank, kind: home|page, bytes}
    <out>/sample.json          the seed, ranks and the sampled sites (for reproducibility)

    python3 -I benchmarks/common/fresh/make_corpus.py <tranco.zip> <out> [--sites 1500] [--seed N] [--lo 1000] [--hi 200000] [--per-site 2]

Only the Python standard library is used.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import random
import re
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.robotparser
import zipfile
from concurrent.futures import ThreadPoolExecutor
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urljoin, urlsplit

UA = "pagesift-bench/0.12 (+https://github.com/YOLOW92/pagesift)"
TIMEOUT = 15
MAX_BYTES = 8 * 1024 * 1024
WORKERS = 24
BARRIER = re.compile(rb"captcha|cf-chl|just a moment|access denied|attention required|are you a robot|"
                     rb"enable javascript and cookies|request unsuccessful", re.I)
SKIP_PATH = re.compile(r"/(login|signin|sign-in|register|signup|cart|checkout|account|search|tag|tags|category|"
                       r"author|privacy|terms|cookie|contact|about|help|faq)(/|$)|\.(pdf|jpg|png|gif|zip|mp4|xml)$", re.I)


class Links(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.hrefs: list[str] = []

    def handle_starttag(self, tag, attrs):
        if tag == "a":
            for k, v in attrs:
                if k == "href" and v:
                    self.hrefs.append(v)


robots: dict[str, urllib.robotparser.RobotFileParser | None] = {}
robots_lock = threading.Lock()


def allowed(url: str) -> bool:
    p = urlsplit(url)
    origin = f"{p.scheme}://{p.netloc}"
    with robots_lock:
        if origin in robots:
            rp = robots[origin]
            return rp is None or rp.can_fetch(UA, url)
    rp = urllib.robotparser.RobotFileParser()
    try:
        req = urllib.request.Request(origin + "/robots.txt", headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            rp.parse(r.read(512 * 1024).decode("utf-8", "replace").splitlines())
    except urllib.error.HTTPError as e:
        rp = None if 400 <= e.code < 500 else rp  # 4xx: no robots.txt, everything allowed
        if rp is not None:
            rp.disallow_all = True
    except Exception:
        rp = None
    with robots_lock:
        robots[origin] = rp
    return rp is None or rp.can_fetch(UA, url)


def fetch(url: str) -> tuple[str, bytes, str] | None:
    if not allowed(url):
        return None
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "text/html,application/xhtml+xml"})
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            ctype = r.headers.get("content-type", "")
            if r.status != 200 or "html" not in ctype:
                return None
            body = r.read(MAX_BYTES + 1)
            if len(body) > MAX_BYTES:
                return None
            return r.geturl(), body, ctype
    except Exception:
        return None


def usable(body: bytes) -> bool:
    return 5_000 <= len(body) and b"<a " in body and not BARRIER.search(body[:200_000])


def content_links(base: str, body: bytes, k: int, rng: random.Random) -> list[str]:
    parser = Links()
    try:
        parser.feed(body.decode("utf-8", "replace"))
    except Exception:
        return []
    host = urlsplit(base).hostname
    seen, out = set(), []
    for h in parser.hrefs:
        u = urljoin(base, h).split("#")[0]
        p = urlsplit(u)
        if p.scheme not in ("http", "https") or p.hostname != host or u in seen:
            continue
        # content pages are usually at least two path segments deep or end in a slug
        if p.path.count("/") < 2 or len(p.path) < 12 or SKIP_PATH.search(p.path):
            continue
        seen.add(u)
        out.append(u)
    rng.shuffle(out)
    return out[:k]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("tranco", type=Path)
    ap.add_argument("out", type=Path)
    ap.add_argument("--sites", type=int, default=1500)
    ap.add_argument("--seed", type=int, default=int(time.time()))
    ap.add_argument("--lo", type=int, default=1000)
    ap.add_argument("--hi", type=int, default=200_000)
    ap.add_argument("--per-site", type=int, default=2)
    a = ap.parse_args()

    z = zipfile.ZipFile(a.tranco)
    rows = list(csv.reader(io.TextIOWrapper(z.open(z.namelist()[0]))))[a.lo - 1 : a.hi]
    rng = random.Random(a.seed)
    sites = rng.sample(rows, a.sites)
    (a.out / "pages").mkdir(parents=True, exist_ok=True)
    (a.out / "sample.json").write_text(json.dumps({"seed": a.seed, "lo": a.lo, "hi": a.hi, "sites": sites}))

    lock = threading.Lock()
    manifest = (a.out / "manifest.jsonl").open("w")
    counter = [0]
    done = [0]

    def save(url, final, body, rank, kind):
        with lock:
            counter[0] += 1
            pid = f"{counter[0]:05d}"
            (a.out / "pages" / f"{pid}.html").write_bytes(body)
            manifest.write(json.dumps({"id": pid, "url": url, "final_url": final, "site_rank": int(rank),
                                       "kind": kind, "bytes": len(body)}) + "\n")
            manifest.flush()

    def site(row):
        rank, domain = row[0], row[1]
        local = random.Random(f"{a.seed}-{domain}")
        try:
            home = f"https://{domain}/"
            r = fetch(home)
            if not r or not usable(r[1]):
                return
            final, body, _ = r
            save(home, final, body, rank, "home")
            kept = 0
            # 候选多取几个，有的会被 robots、类型或拦截页过滤掉
            for u in content_links(final, body, a.per_site * 3, local):
                if kept >= a.per_site:
                    break
                time.sleep(1.0)  # one request at a time per host, with a pause
                rr = fetch(u)
                if rr and usable(rr[1]):
                    save(u, rr[0], rr[1], rank, "page")
                    kept += 1
        finally:
            with lock:
                done[0] += 1
                if done[0] % 100 == 0:
                    print(f"  {done[0]}/{len(sites)} sites, {counter[0]} pages", file=sys.stderr)

    with ThreadPoolExecutor(WORKERS) as pool:
        list(pool.map(site, sites))
    manifest.close()
    print(f"{counter[0]} pages from {len(sites)} sampled sites (seed {a.seed})", file=sys.stderr)


if __name__ == "__main__":
    main()
