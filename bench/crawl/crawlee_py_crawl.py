"""Crawlee Python (BeautifulSoupCrawler) 整站爬取：每个站点从首页出发，遵守 robots.txt，每站最多 pages 页。

<venv>/bin/python -I crawlee_py_crawl.py <port> <hosts> <pages> <interval-ms> [per-host]
"""
import asyncio
from collections import Counter
from datetime import timedelta
import logging
import os
import shutil
import sys
import tempfile
import time
from urllib.parse import urlsplit

from crawlee import ConcurrencySettings
from crawlee.crawlers import BeautifulSoupCrawler, BeautifulSoupCrawlingContext
from crawlee.storage_clients import MemoryStorageClient

logging.disable(logging.CRITICAL)


async def main() -> None:
    port, hosts, pages, interval = (int(x) for x in sys.argv[1:5])
    per_host = int(sys.argv[5]) if len(sys.argv) > 5 else 2
    start_host = int(sys.argv[6]) if len(sys.argv) > 6 else 0
    domains = {f"s{i}.localhost" for i in range(start_host, start_host + hosts)}
    counts: Counter[str] = Counter()
    total_chars = 0
    total_err = 0

    t0 = time.monotonic()
    max_conc = min(max(hosts * per_host, 16), 256)
    tasks_per_min = (60_000.0 / interval) * hosts if interval > 0 else float("inf")
    crawler = BeautifulSoupCrawler(
        parser="lxml",
        storage_client=MemoryStorageClient(),
        concurrency_settings=ConcurrencySettings(
            min_concurrency=min(max_conc, 64),
            max_concurrency=max_conc,
            desired_concurrency=max_conc,
            max_tasks_per_minute=tasks_per_min,
        ),
        max_requests_per_crawl=hosts * pages + hosts * 2,
        max_request_retries=1,
        max_crawl_depth=100,
        use_session_pool=False,
        respect_robots_txt_file=True,
        request_handler_timeout=timedelta(seconds=15),
        configure_logging=False,
    )

    @crawler.router.default_handler
    async def request_handler(context: BeautifulSoupCrawlingContext) -> None:
        nonlocal total_chars
        url = context.request.loaded_url or context.request.url
        host = urlsplit(url).hostname or ""
        if host not in domains or counts[host] >= pages:
            return
        counts[host] += 1
        total_chars += len(context.html_to_text())
        if counts[host] < pages:
            await context.enqueue_links(strategy="same-origin")

    @crawler.failed_request_handler
    async def failed_handler(context: BeautifulSoupCrawlingContext, error: Exception) -> None:
        nonlocal total_err
        total_err += 1

    start_urls = [f"http://s{i}.localhost:{port}/" for i in range(start_host, start_host + hosts)]
    await crawler.run(start_urls)

    elapsed = time.monotonic() - t0
    ok = sum(counts.values())
    print(
        f"crawlee-python: {ok} pages from {hosts} sites in {elapsed:.2f} s = {ok / max(elapsed, 1e-6):.0f} pages/s, "
        f"errors {total_err}, text {total_chars / 1e6:.1f} MB"
    )


if __name__ == "__main__":
    asyncio.run(main())
