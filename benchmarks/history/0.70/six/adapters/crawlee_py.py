#!/usr/bin/env python3
"""Conventional Crawlee Python BeautifulSoupCrawler + enqueue_links; mirrors crawlee.mjs."""
import asyncio
import json
import logging
import os
import shutil
import sys
import tempfile
from datetime import timedelta
from pathlib import Path

from crawlee import ConcurrencySettings
from crawlee.crawlers import BeautifulSoupCrawler, BeautifulSoupCrawlingContext

logging.disable(logging.CRITICAL)


async def main() -> None:
    url, output = sys.argv[1], sys.argv[2]
    pages = []
    errors = []
    storage_dir = tempfile.mkdtemp(prefix="pagesift-crawlee-py-six-")
    os.environ["CRAWLEE_STORAGE_DIR"] = storage_dir
    try:
        crawler = BeautifulSoupCrawler(
            parser="lxml",
            concurrency_settings=ConcurrencySettings(min_concurrency=1, max_concurrency=2, desired_concurrency=2),
            max_requests_per_crawl=50,
            max_request_retries=1,
            max_crawl_depth=4,
            use_session_pool=False,
            respect_robots_txt_file=True,
            request_handler_timeout=timedelta(seconds=10),
            configure_logging=False,
        )

        @crawler.router.default_handler
        async def request_handler(context: BeautifulSoupCrawlingContext) -> None:
            h1 = context.soup.find("h1")
            body = context.soup.find("body")
            pages.append(
                {
                    "url": context.request.loaded_url or context.request.url,
                    "title": h1.get_text() if h1 else "",
                    "body_chars": len(body.get_text()) if body else 0,
                }
            )
            await context.enqueue_links(strategy="same-origin")

        @crawler.failed_request_handler
        async def failed_handler(context: BeautifulSoupCrawlingContext, error: Exception) -> None:
            errors.append({"url": context.request.url, "error": str(error)})

        await crawler.run([url])
        Path(output).write_text(json.dumps({"pages": pages, "errors": errors}, indent=2) + "\n")
    finally:
        shutil.rmtree(storage_dir, ignore_errors=True)


if __name__ == "__main__":
    asyncio.run(main())
