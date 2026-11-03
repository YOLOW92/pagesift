"""Collect the corpus for the 0.93 relocation benchmark (run once; output under benchmarks/data/0.93/).

- live/: current pages fetched politely (robots.txt checked, one request per PAUSE seconds,
  identifying User-Agent). These are the bases for the synthetic redesigns.
- cc/: the same URL from two Common Crawl crawls years apart (real redesigns), via cc.py.
  The raw WARC members are kept too (cc/sample.warc.gz): 0.94 uses them to test the WARC reader.

    python -I fetch.py
"""

import json
import pathlib
import ssl
import sys
import time
import urllib.parse
import urllib.request
import urllib.robotparser

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import cc  # noqa: E402

OUT = pathlib.Path(__file__).resolve().parents[2] / "data" / "0.93"
UA = cc.UA
CTX = cc.CTX
PAUSE = 2.0

LIVE = [
    "https://books.toscrape.com/",
    "https://books.toscrape.com/catalogue/a-light-in-the-attic_1000/index.html",
    "https://quotes.toscrape.com/",
    "https://quotes.toscrape.com/tag/love/",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://en.wikipedia.org/wiki/Web_scraping",
    "https://en.wikipedia.org/wiki/Markdown",
    "https://docs.python.org/3/library/json.html",
    "https://www.rust-lang.org/",
    "https://www.python.org/",
    "https://www.gnu.org/",
    "https://www.debian.org/",
    "https://www.apache.org/",
    "https://www.w3.org/",
    "https://www.kernel.org/",
    "https://www.php.net/",
    "https://www.ruby-lang.org/en/",
    "https://nodejs.org/en",
    "https://go.dev/",
    "https://www.gov.uk/",
    "https://news.ycombinator.com/",
    "https://lobste.rs/",
    "https://arxiv.org/abs/1706.03762",
    "https://www.postgresql.org/",
    "https://www.sqlite.org/index.html",
    "https://crates.io/",
    "https://pypi.org/project/requests/",
    "https://developer.mozilla.org/en-US/docs/Web/HTML",
    "https://www.openbsd.org/",
    "https://www.freebsd.org/",
]

CC_A, CC_B = "CC-MAIN-2017-13", "CC-MAIN-2024-10"
CC_URLS = [
    "https://www.python.org/",
    "https://www.rust-lang.org/",
    "https://www.gnu.org/",
    "https://www.debian.org/",
    "https://www.apache.org/",
    "https://www.w3.org/",
    "https://www.kernel.org/",
    "https://www.php.net/",
    "https://www.ruby-lang.org/en/",
    "https://www.postgresql.org/",
    "https://www.sqlite.org/index.html",
    "https://www.gov.uk/",
    "https://news.ycombinator.com/",
    "https://www.openbsd.org/",
    "https://www.freebsd.org/",
    "https://www.perl.org/",
    "https://www.mozilla.org/en-US/",
    "https://www.nasa.gov/",
    "https://www.npr.org/",
    "https://www.wikipedia.org/",
    "https://www.djangoproject.com/",
    "https://www.ietf.org/",
    "https://www.eff.org/",
    "https://www.archlinux.org/",
    "https://www.ubuntu.com/",
    "https://www.fsf.org/",
    "https://www.haskell.org/",
    "https://www.scala-lang.org/",
    "https://www.lua.org/",
    "https://www.nginx.com/",
]


def name(url):
    return urllib.parse.quote(url, safe="")[:150]


def allowed(url, cache={}):
    u = urllib.parse.urlsplit(url)
    root = f"{u.scheme}://{u.netloc}"
    if root not in cache:
        rp = urllib.robotparser.RobotFileParser()
        try:
            req = urllib.request.Request(root + "/robots.txt", headers={"User-Agent": UA})
            with urllib.request.urlopen(req, context=CTX, timeout=30) as r:
                rp.parse(r.read().decode("utf-8", "replace").splitlines())
        except Exception:
            rp.parse([])
        cache[root] = rp
    return cache[root].can_fetch(UA, url)


def live():
    d = OUT / "live"
    d.mkdir(parents=True, exist_ok=True)
    for url in LIVE:
        path = d / (name(url) + ".html")
        if path.exists():
            continue
        if not allowed(url):
            print("robots disallow", url)
            continue
        time.sleep(PAUSE)
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, context=CTX, timeout=60) as r:
                path.write_bytes(r.read())
            print("ok", url)
        except Exception as e:
            print("fail", url, e)


def common_crawl():
    d = OUT / "cc"
    d.mkdir(parents=True, exist_ok=True)
    pairs = []
    sample = open(d / "sample.warc.gz", "ab")
    for url in CC_URLS:
        out = d / (name(url) + ".json")
        if out.exists():
            pairs.append(json.loads(out.read_text()))
            continue
        pair = {"url": url}
        for label, crawl in (("old", CC_A), ("new", CC_B)):
            try:
                recs = cc.lookup(crawl, url)
            except Exception as e:
                print("lookup fail", crawl, url, e)
                recs = []
            if not recs:
                break
            member = cc.fetch_record(recs[0])
            sample.write(member)
            pair[label] = cc.html_of(member)
            pair[label + "_crawl"] = crawl
        if pair.get("old") and pair.get("new"):
            out.write_text(json.dumps(pair))
            pairs.append(pair)
            print("pair", url)
        else:
            print("no pair", url)
    sample.close()
    return pairs


if __name__ == "__main__":
    live()
    common_crawl()
