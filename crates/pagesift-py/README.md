# pagesift (Python)

0.20.0: see the [release and migration guide](https://github.com/YOLOW92/pagesift/blob/main/docs/releases/0.20.0.md). New collection/archive cores are available through Rust and CLI; this package keeps its documented scraping interface.

Python bindings for [pagesift](https://github.com/YOLOW92/pagesift): turn web pages into clean
Markdown for LLMs. Fetching, robots.txt, rate limiting, content extraction, PDF parsing and browser
rendering all run in Rust. No Playwright, no Rust toolchain needed to install.

pagesift follows the rules: it obeys robots.txt and Crawl-delay, rate-limits per host, and never
bypasses anti-bot protection. A blocked page is reported as an error (`AccessBarrier`), not retried
with tricks.

## Quick start

```python
import pagesift

doc = pagesift.scrape("https://example.com/post")
print(doc.markdown, doc.metadata["title"], doc.status_code)

# Many pages: one call, concurrent in Rust, results as they finish
with pagesift.Scraper() as scraper:          # keep one Scraper for the life of the program
    for doc in scraper.scrape_iter(urls, concurrency=16):
        if doc.success:
            save(urls[doc.index], doc.markdown)
        else:
            print(doc.url, doc.error_code, doc.error_message)
```

Async:

```python
async with pagesift.Scraper() as scraper:
    doc = await scraper.ascrape("https://example.com")
    async for doc in scraper.ascrape_many(urls, concurrency=16):   # completion order
        ...
```

Convert HTML you already have (no network):

```python
doc = pagesift.convert(html, url="https://example.com/post", only_main_content=True)
```

## Designed for throughput

- **Batch first.** `scrape_many`, `scrape_iter` and `ascrape_many` take the whole list in one call;
  the concurrency runs on Rust's async runtime, not in Python threads or Python tasks.
- **Stream in completion order.** `scrape_iter` / `ascrape_many` hand you each page as soon as it
  is done, so one slow site does not hold back the rest. `doc.index` maps a result back to its URL.
- **Copy only what you read.** A `Document` keeps its fields as Rust values. A field becomes a
  Python object the first time you read it (strings are then cached), so `raw_html` you never
  touch costs nothing. Failed items build their exception object only when you read `doc.error`.
- **Request only what you need.** Formats you do not ask for (`html`, `raw_html`, `links`,
  `images`) are never computed.
- **Reuse one `Scraper`.** It owns the connection pool, the robots.txt cache, rate-limit state and
  the browser pool. The module-level functions share one default `Scraper` per process.
- **The GIL is released** while pages are fetched and processed, so even plain Python threads
  scale.

### Benchmarks

`python crates/pagesift-py/bench/bench.py` (release build, local HTTP server in a separate process,
no internet). 16-thread Linux machine, Python 3.14; the machine was also running other builds
(load average ≈ 15), so absolute numbers are noisy, the comparisons are what matter.

**1. 200 pages, 50 ms server latency each**

| method | pages/s (higher is better) | CPU seconds in the calling process (lower is better) |
|---|---:|---:|
| `scrape` in a loop | 19 | 0.22 |
| `scrape` × 16 Python threads | 296 | 0.18 |
| `ascrape` × `asyncio.gather` (16 at a time) | 294 | 0.23 |
| `scrape_many(concurrency=16)` | 295 | 0.17 |
| `ascrape_many(concurrency=16)` | 296 | 0.18 |

One `scrape_many` call is 15× faster than a loop and as fast as a hand-built thread pool or
`asyncio.gather`, with no threads, semaphores or task bookkeeping on the Python side. At this
latency the server is the limit (16 × 1000 / 50 ms = 320 pages/s at most).

**2. Time to first result: 1 slow page (2 s) + 31 fast pages**

| method | first result, ms (lower is better) | all results, ms (lower is better) |
|---|---:|---:|
| `scrape_many` (input order) | 2002 | 2002 |
| `scrape_iter` (completion order) | 23 | 2001 |

**3. Lazy fields: 100 pages of ~200 KB, all formats requested**

| what the caller reads | Python-side µs per page (lower is better) |
|---|---:|
| `success` + `content_hash` only | 0.4 |
| `markdown` only | 69.2 |
| every field (`to_dict()`) | 235.9 |

Converting everything up front would cost every caller the last row; with lazy fields you pay for
what you read.

**4. Reusing one `Scraper` vs a new one per page (40 pages, sequential)**

| method | ms per page (lower is better) |
|---|---:|
| one long-lived `Scraper` | 0.99 |
| new `Scraper` per page | 3.21 |

On a real network the gap is larger: a new `Scraper` also redoes DNS, TCP and TLS handshakes and
refetches robots.txt.

**5. `convert` on ~200 KB pages with Python threads (GIL released)**

| threads | pages/s (higher is better) |
|---:|---:|
| 1 | 330 |
| 4 | 1254 |
| 8 | 2083 |

## API

| | |
|---|---|
| `scrape(url, options=None, **overrides) -> Document` | one page; raises a `PagesiftError` subclass on failure |
| `await ascrape(url, options=None, **overrides) -> Document` | async `scrape` |
| `scrape_many(urls, options=None, *, concurrency=16, **overrides) -> list[Document]` | waits for all, input order |
| `scrape_iter(urls, options=None, *, concurrency=16, **overrides) -> Batch` | `for doc in …`, completion order |
| `ascrape_many(urls, options=None, *, concurrency=16, **overrides) -> Batch` | `async for doc in …`, completion order |
| `convert(html, url=None, only_main_content=True) -> Document` | HTML → Markdown, no network |
| `Scraper(...)` | all of the above as methods, plus `close()` / `aclose()`; `with` / `async with` |
| `default_scraper()` | the `Scraper` the module-level functions use |
| `ScrapeOptions(...)` | validated, reusable options; `.replace(**changes)` |

In batch calls one failed URL never fails the batch: its `Document` has `success=False` and
`error` set. A `Batch` starts scraping as soon as it is created, can be iterated once (with `for`
or `async for`), supports `len()`, and `batch.cancel()` (or dropping it) cancels unfinished pages.

### Options

Pass a `ScrapeOptions`, keyword arguments, or both (keywords override fields of the object).
Invalid values raise immediately (`InvalidRequest` / `UnsupportedOption`, both also `ValueError`);
unknown keywords raise `TypeError`.

| option | default | |
|---|---|---|
| `formats` | `("markdown",)` | any of `markdown`, `html`, `raw_html`, `links`, `images` |
| `only_main_content` | `True` | drop navigation, headers, footers, sidebars |
| `engine` | `"auto"` | `http`, `browser`, or `auto` (HTTP first, render in Chrome only when the page has too little text) |
| `timeout` | `None` | total timeout, milliseconds (at least 1000) |
| `wait_for` | `0` | extra wait after browser rendering, milliseconds |
| `headers` | `None` | extra request headers |
| `include_tags`, `exclude_tags` | `None` | not supported yet: raises `UnsupportedOption` rather than being ignored |
| `ignore_robots_txt` | `False` | skip robots.txt for this scrape |
| `mobile` | `False` | mobile emulation when rendering |
| `languages` | `None` | `Accept-Language` when rendering, e.g. `["de-DE", "en"]` |

### `Scraper(...)`

| setting | default | |
|---|---|---|
| `user_agent` | pagesift's own | browser-impersonating user agents are refused |
| `allow_private_network` | `False` | SSRF protection: local and private addresses are refused unless allowed |
| `request_interval` | `1.0` | seconds between requests to the same host; robots.txt Crawl-delay wins |
| `per_host_concurrency` | `2` | |
| `max_concurrency` | `64` | across all hosts |
| `browser_executable` / `browser_url` | auto-detect | a Chrome binary, or a running browser (`ws://…`, `http://host:port`) |
| `max_browsers` | `2` | browser processes in the pool |

`concurrency=` on batch calls caps pages in flight for that call; the per-host limits above still
apply, so a batch against one host is as polite as single requests.

### `Document`

| field | |
|---|---|
| `index` | position in the batch input (`None` for single scrapes) |
| `url`, `final_url` | requested URL, URL after redirects |
| `success`, `status_code` | HTTP 4xx/5xx pages are not errors: you get the page and its `status_code` |
| `markdown`, `html`, `raw_html`, `links`, `images` | `None` unless requested in `formats` |
| `metadata` | dict: `title`, `description`, `language`, `keywords`, `robots`, `favicon`, `published_time`, `modified_time`, `content_type`, plus other meta tags such as `og_image` |
| `error`, `error_code`, `error_message` | set when `success` is `False` |
| `engine` | `http` or `browser` |
| `content_hash`, `raw_hash`, `text_hash` | content fingerprints for change detection |
| `elapsed_ms`, `timings` | total and per-phase time, milliseconds |
| `warnings`, `redirect_chain`, `etag`, `last_modified`, `not_modified` | |

`to_dict()` converts every field at once; reading attributes is cheaper when you need only some.

### Errors

Every error code has its own exception class; all derive from `pagesift.PagesiftError` and carry
`code`, `message`, `details` (dict), `retry_after` (seconds or `None`) and `url`.

| class | code | when |
|---|---|---|
| `RobotsDisallowed` | `ROBOTS_DISALLOWED` | robots.txt forbids the URL |
| `AccessBarrier` | `ACCESS_BARRIER` | the site served a CAPTCHA / challenge / block page; `details` has `vendor`, `kind`, `confidence` |
| `RateLimited` | `RATE_LIMITED` | the site answered 429 |
| `HostCooldown` | `HOST_COOLDOWN` | the host failed or throttled us recently; retry after `retry_after` |
| `Timeout` | `TIMEOUT` | also a `TimeoutError` |
| `SsrfBlocked` | `SSRF_BLOCKED` | private, loopback or metadata address |
| `TooLarge` | `TOO_LARGE` | response over the size limit |
| `UpstreamError` | `UPSTREAM_ERROR` | the site or the renderer failed (bad response, unreadable PDF, browser crash) |
| `NetworkError` | `NETWORK_ERROR` | connection refused or reset, TLS failure, navigation error; subclass of `UpstreamError` |
| `DnsFailure` | `DNS_FAILURE` | the host name does not resolve; retry later; subclass of `UpstreamError` |
| `Busy` | `BUSY` | no free browser within the request's deadline; retry later |
| `UnsupportedOption` | `UNSUPPORTED_OPTION` | asked for something this build cannot do; also a `ValueError` |
| `InvalidRequest` | `INVALID_REQUEST` | bad URL or option; also a `ValueError` |
| `InternalError` | `INTERNAL_ERROR` | a bug in pagesift |

## Development

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install maturin pytest
maturin develop -m crates/pagesift-py/Cargo.toml
pytest crates/pagesift-py/tests
```

The tests and the benchmark start a local HTTP server and never touch the internet.

## License

AGPL-3.0-only, with a commercial license available for use without AGPL obligations. See the repository's [LICENSE](../../LICENSE) and [COMMERCIAL.md](../../COMMERCIAL.md).
