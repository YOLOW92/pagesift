"""Python 接口设计的性能收益：本地 HTTP 服务器（子进程，模拟网络延迟），不访问外网。

    maturin develop --release -m crates/pagesift-py/Cargo.toml
    python crates/pagesift-py/bench/bench.py
"""

from __future__ import annotations

import asyncio
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PARA = (
    "<p>Paragraph {i} of a normal article, with enough words to read like real prose "
    "that someone wrote for people rather than for robots or crawlers.</p>"
)


def page(n: int) -> bytes:
    body = "".join(PARA.format(i=i) for i in range(n))
    nav = "".join(f'<a href="/l/{i}">link {i}</a>' for i in range(200))
    return (
        f"<!DOCTYPE html><html><head><title>Bench</title></head><body><nav>{nav}</nav>"
        f"<article><h1>Bench</h1>{body}</article><footer>footer</footer></body></html>"
    ).encode()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a: object) -> None:
        pass

    def do_GET(self) -> None:
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path == "/robots.txt":
            data, ctype = b"User-agent: *\nAllow: /\n", "text/plain"
        else:
            time.sleep(int(q.get("ms", ["0"])[0]) / 1000)
            data, ctype = page(int(q.get("p", ["40"])[0])), "text/html; charset=utf-8"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 512

    def handle_error(self, *a: object) -> None:
        pass


def serve() -> None:
    httpd = Server(("127.0.0.1", 0), Handler)
    print(httpd.server_address[1], flush=True)
    httpd.serve_forever()


def timed(f):
    t, c = time.perf_counter(), time.process_time()
    r = f()
    return r, time.perf_counter() - t, time.process_time() - c


def row(name: str, *cols: str) -> None:
    print(f"| {name} | " + " | ".join(cols) + " |")


def main() -> None:
    import pagesift

    proc = subprocess.Popen([sys.executable, __file__, "--serve"], stdout=subprocess.PIPE, text=True)
    base = f"http://127.0.0.1:{proc.stdout.readline().strip()}"
    try:
        run(pagesift, base)
    finally:
        proc.terminate()


def run(pagesift, base: str) -> None:
    opts = pagesift.ScrapeOptions(engine="http")
    mk = lambda: pagesift.Scraper(  # noqa: E731
        allow_private_network=True, request_interval=0, per_host_concurrency=32, max_concurrency=64
    )
    s = mk()
    s.scrape(f"{base}/warm", opts)

    # 1. 批量：200 页，每页 50ms 延迟
    n, lat = 200, 50
    urls = [f"{base}/b?ms={lat}&i={i}" for i in range(n)]
    print(f"\n### 1. {n} pages, {lat} ms server latency each\n")
    row("method", "pages/s (higher is better)", "CPU s in this process (lower is better)")
    row("---", "---:", "---:")
    _, w, c = timed(lambda: [s.scrape(u, opts) for u in urls[:40]])
    row("`scrape` in a loop", f"{40 / w:.0f}", f"{c * n / 40:.2f}")
    with ThreadPoolExecutor(16) as pool:
        _, w, c = timed(lambda: list(pool.map(lambda u: s.scrape(u, opts), urls)))
    row("`scrape` × 16 Python threads", f"{n / w:.0f}", f"{c:.2f}")

    async def gather():
        sem = asyncio.Semaphore(16)

        async def one(u):
            async with sem:
                return await s.ascrape(u, opts)

        return await asyncio.gather(*(one(u) for u in urls))

    _, w, c = timed(lambda: asyncio.run(gather()))
    row("`ascrape` × `asyncio.gather` (16 at a time)", f"{n / w:.0f}", f"{c:.2f}")
    _, w, c = timed(lambda: s.scrape_many(urls, opts, concurrency=16))
    row("`scrape_many(concurrency=16)`", f"{n / w:.0f}", f"{c:.2f}")

    async def amany():
        return [d async for d in s.ascrape_many(urls, opts, concurrency=16)]

    _, w, c = timed(lambda: asyncio.run(amany()))
    row("`ascrape_many(concurrency=16)`", f"{n / w:.0f}", f"{c:.2f}")

    # 2. 第一个结果多久拿到：1 个慢页面（2 秒）+ 31 个快页面
    print("\n### 2. Time to first result: 1 slow page (2 s) + 31 fast pages\n")
    row("method", "first result, ms (lower is better)", "all results, ms (lower is better)")
    row("---", "---:", "---:")
    mixed = [f"{base}/slow?ms=2000"] + [f"{base}/f?ms=20&i={i}" for i in range(31)]
    t = time.perf_counter()
    s.scrape_many(mixed, opts, concurrency=8)
    allms = (time.perf_counter() - t) * 1000
    row("`scrape_many` (input order)", f"{allms:.0f}", f"{allms:.0f}")
    t = time.perf_counter()
    first = None
    for _ in s.scrape_iter(mixed, opts, concurrency=8):
        first = first or (time.perf_counter() - t) * 1000
    row("`scrape_iter` (completion order)", f"{first:.0f}", f"{(time.perf_counter() - t) * 1000:.0f}")

    # 3. 按需转换：每页约 200 KB，请求全部格式
    print("\n### 3. Lazy fields: 100 pages of ~200 KB, all formats requested\n")
    row("what the caller reads", "Python-side µs per page (lower is better)")
    row("---", "---:")
    big = [f"{base}/big?p=1200&i={i}" for i in range(100)]
    full = opts.replace(formats=["markdown", "html", "raw_html", "links", "images"])
    for name, read in [
        ("`success` + `content_sha256` only", lambda d: (d.success, d.content_sha256)),
        ("`markdown` only", lambda d: d.markdown),
        ("every field (`to_dict()`)", lambda d: d.to_dict()),
    ]:
        docs = s.scrape_many(big, full, concurrency=16)
        t = time.perf_counter()
        for d in docs:
            read(d)
        row(name, f"{(time.perf_counter() - t) / len(docs) * 1e6:.1f}")

    # 4. 复用 Scraper：连接池和 robots.txt 缓存
    print("\n### 4. Reusing one `Scraper` vs a new one per page (40 pages, sequential)\n")
    row("method", "ms per page (lower is better)")
    row("---", "---:")
    few = [f"{base}/r?i={i}" for i in range(40)]
    _, w, _ = timed(lambda: [s.scrape(u, opts) for u in few])
    row("one long-lived `Scraper`", f"{w / len(few) * 1000:.2f}")

    def fresh():
        for u in few:
            with mk() as t:
                t.scrape(u, opts)

    _, w, _ = timed(fresh)
    row("new `Scraper` per page", f"{w / len(few) * 1000:.2f}")

    # 5. convert 释放 GIL：Python 线程能用满多核
    print("\n### 5. `convert` on ~200 KB pages with Python threads (GIL released)\n")
    row("threads", "pages/s (higher is better)")
    row("---", "---:")
    html = page(1200).decode()
    k = 64
    for threads in (1, 4, 8):
        with ThreadPoolExecutor(threads) as pool:
            _, w, _ = timed(lambda: list(pool.map(pagesift.convert, [html] * k)))
        row(str(threads), f"{k / w:.0f}")
    print(f"\n(cpu count: {os.cpu_count()}, pagesift {pagesift.__version__}, Python {sys.version.split()[0]})")


if __name__ == "__main__":
    if sys.argv[1:] == ["--serve"]:
        serve()
    else:
        main()
