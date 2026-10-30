#!/usr/bin/env python3
"""End-to-end check that the official Firecrawl Python SDK works against `pagesift serve` unmodified.

    pagesift serve --port 3999 --api-key test-key &
    pip install firecrawl-py
    python scripts/firecrawl_sdk_compat.py

Uses live sites (one request each, robots.txt respected). Exits non-zero on any mismatch.
"""

import sys

from firecrawl import Firecrawl

URL = "http://localhost:3999"
fc = Firecrawl(api_key="test-key", api_url=URL)
failures = []


def expect(cond, what):
    print(("ok   " if cond else "FAIL ") + what)
    if not cond:
        failures.append(what)


doc = fc.scrape("https://blog.rust-lang.org/2024/11/28/Rust-1.83.0/", formats=["markdown", "links"])
expect(doc.metadata.status_code == 200, "scrape returns statusCode 200")
expect("Rust 1.83.0" in (doc.markdown or ""), "scrape returns the article markdown")
expect(len(doc.links or []) > 10, "links format returns links")
expect("contentHash" in doc.metadata.extras.get("pagesift", {}), "pagesift extension is readable via extras")

for name, call, exc in [
    ("proxy stealth is rejected", lambda: fc.scrape("https://example.com", proxy="stealth"), "BadRequestError"),
    ("robots disallow maps to 403", lambda: fc.scrape("https://www.google.com/search?q=x"), "WebsiteNotSupportedError"),
    ("SSRF is rejected", lambda: fc.scrape("http://127.0.0.1/"), "BadRequestError"),
    ("bad API key", lambda: Firecrawl(api_key="wrong", api_url=URL).scrape("https://example.com"), "UnauthorizedError"),
]:
    try:
        call()
        expect(False, f"{name} (no error raised)")
    except Exception as e:
        expect(type(e).__name__ == exc, f"{name} ({type(e).__name__})")

job = fc.batch_scrape(["https://example.com", "https://www.rust-lang.org/"], formats=["markdown"], poll_interval=1)
expect(job.status == "completed" and job.completed == 2 and len(job.data) == 2, "batch scrape completes with 2 documents")

sys.exit(1 if failures else 0)
