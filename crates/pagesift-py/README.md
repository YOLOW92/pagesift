# pagesift (Python)

1.0.0: see the [release notes](https://github.com/YOLOW92/pagesift/blob/main/docs/releases/1.0.0.md).

Python bindings for [pagesift](https://github.com/YOLOW92/pagesift): turn web pages into clean
Markdown for LLMs. Fetching, robots.txt, rate limiting, content extraction, PDF parsing and browser
rendering all run in Rust. No Playwright, no Rust toolchain needed to install.

pagesift obeys robots.txt and Crawl-delay and rate-limits per host; the per-site budget is shared by
every egress. A blocked or challenge page is reported as an error (`AccessBarrier`). When two or more
egresses are configured (`[[egress]]`), a page denied through one egress (an IP block or a challenge
page) can be retried through another; rate limiting (429, `Retry-After`) only backs off.

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
| `scrape_many(urls, options=None, *, concurrency=16, request_interval_ms=None, per_host_concurrency=None, max_concurrency=None, **overrides) -> list[Document]` | waits for all, input order; module-level rate settings use a temporary scraper |
| `scrape_iter(urls, options=None, *, concurrency=16, **overrides) -> Batch` | `for doc in …`, completion order |
| `ascrape_many(urls, options=None, *, concurrency=16, **overrides) -> Batch` | `async for doc in …`, completion order |
| `convert(html, url=None, only_main_content=True) -> Document` | HTML → Markdown, no network |
| `Scraper(...)` | all of the above as methods, plus `close()` / `aclose()`; `with` / `async with` |
| `Scraper.crawl(url, options=None, *, limit=None, max_depth=None, include_paths=None, exclude_paths=None, crawl_entire_domain=False, allow_subdomains=None, sitemap="include", query=None, adaptive=False, max_concurrency=None, **overrides) -> Crawl` | whole-site crawl; `for` / `async for`, one `Document` per finished page |
| `Scraper.map(url, *, limit=None, search=None, sitemap="include", ignore_robots_txt=False) -> list[dict]` | the site's links as `{"url", "title", "description"}`; `await amap(...)` is the async form |
| `Scraper.parse(data, filename, options=None, *, content_type=None, url=None, **overrides) -> Document` | parse a local file (PDF, HTML, Word, Excel, PowerPoint, EPUB, CSV); `await aparse(...)` is the async form |
| `Scraper.parse_many(files, options=None, *, concurrency=16, **overrides) -> Batch` | `files` are `(data, filename)` pairs; `for` / `async for`, completion order |
| `default_scraper()` | the `Scraper` the module-level functions use |
| `ScrapeOptions(...)` | validated, reusable options; `.replace(**changes)` |

In batch calls one failed URL never fails the batch: its `Document` has `success=False` and
`error` set. A `Batch` starts scraping as soon as it is created, can be iterated once (with `for`
or `async for`), supports `len()`, and `batch.cancel()` (or dropping it) cancels unfinished pages.
`concurrency` controls batch tasks; per-host concurrency and request spacing are separate limits,
and a robots.txt `Crawl-delay` takes precedence over the configured request interval. Use
`engine="http"` for HTTP-only batches; the default `engine="auto"` preserves browser fallback for
short or client-rendered pages.

### Crawl, map, parse

`crawl` starts as soon as it is called and yields pages as they finish. Failed pages are yielded
too (`success=False`, same shape as failed batch items; `index` is `None`); an error in the crawl
itself (for example an invalid `include_paths` regex) is raised from the iteration. Results go
through a bounded channel whose capacity is the crawl's concurrency: when the consumer is slower,
the crawl waits instead of dropping pages. `break` (dropping the iterator) or `crawl.cancel()`
stops the crawl. After the iteration ends, `crawl.summary` is a dict with `stop_reason`, `pages`,
`coverage` and `term_evidence`; crawls with `query` get it from the engine, other crawls report
`limit_reached`, `exhausted` or `cancelled` from the page counts. `crawl.total`, `crawl.pages` and
`crawl.errors` are live counters.

| crawl option | default | |
|---|---|---|
| `limit` | HTTP API default | maximum pages; with `adaptive` only a cap |
| `max_depth` | `None` | link depth from the start page |
| `include_paths`, `exclude_paths` | `None` | path regexes: crawl only / skip matching pages |
| `crawl_entire_domain` | `False` | the whole host, not just the start page's section |
| `allow_subdomains` | `None` | also crawl subdomains of the same site |
| `sitemap` | `"include"` | `include`, `skip` (no sitemap) or `only` (sitemap URLs only) |
| `query`, `adaptive` | `None`, `False` | relevance-first crawl; `adaptive` stops once the query is covered (needs `query`) |
| `max_concurrency` | `None` | pages in flight for this crawl |
| `options`, `**overrides` | | per-page scrape options; `ignore_robots_txt` also applies to the crawl itself |

`map` orders links that contain more `search` terms first and keeps the rest after them.
`parse` detects the type from the content first, then from the file name extension and
`content_type`; unknown types raise `UnsupportedOption`, as do options that only make sense for
URLs (`engine="browser"`, `wait_for`, `mobile`, conditional requests). `url` resolves relative
links in HTML; it is never fetched. A parsed `Document` has the file name as `url`.

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
| `include_tags`, `exclude_tags` | `None` | CSS selectors (tag, `.class`, `#id`, …): keep only / drop matching elements before extraction; an invalid selector raises `InvalidRequest` |
| `ignore_robots_txt` | `False` | skip robots.txt for this scrape |
| `mobile` | `False` | mobile emulation when rendering |
| `languages` | `None` | `Accept-Language` when rendering, e.g. `["de-DE", "en"]` |

### `Scraper(...)`

| setting | default | |
|---|---|---|
| `user_agent` | pagesift's own | browser-impersonating user agents are refused |
| `allow_private_network` | `False` | SSRF protection: local and private addresses are refused unless allowed |
| `request_interval_ms` | `1000` | milliseconds between requests to the same host; robots.txt Crawl-delay wins |
| `ocr` | `False` | OCR scanned pages; models download when the first scanned image arrives |
| `ocr_quality` | `"auto"` | `auto`, `fast` or `accurate` |
| `ocr_formulas` | `False` | also recognize display formulas as `$$…$$`; downloads 860 MB more |
| `models_dir` | cache directory | where OCR models live |
| `gpu` | `"auto"` | `auto`, `off`, `cuda` or `cuda:N`; text models use the GPU, layout and formula models stay on the CPU |
| `per_host_concurrency` | `2` | |
| `max_concurrency` | `64` | across all hosts |
| `browser_executable` / `browser_url` | auto-detect | a Chrome binary, or a running browser (`ws://…`, `http://host:port`) |
| `max_browsers` | `2` | browser processes in the pool |

`concurrency=` on batch calls caps pages in flight for that call; the per-host limits above still
apply, so a batch against one host is as polite as single requests.

### `Document`

| field | |
|---|---|
| `index` | position in the batch input (`None` for single scrapes and crawl pages) |
| `url`, `final_url` | requested URL (the file name for `parse`), URL after redirects |
| `success`, `status_code` | HTTP 4xx/5xx pages are not errors: you get the page and its `status_code` |
| `markdown`, `html`, `raw_html`, `links`, `images` | `None` unless requested in `formats` |
| `metadata` | dict: `title`, `description`, `language`, `keywords`, `robots`, `favicon`, `published_time`, `author`, `modified_time`, `content_type`, plus other meta tags such as `og_image` (same keys for `scrape` and `convert`) |
| `error`, `error_code`, `error_message` | set when `success` is `False` |
| `engine` | `http` or `browser` |
| `content_hash`, `raw_hash`, `text_hash` | content fingerprints for change detection |
| `elapsed_ms`, `timings` | total and per-phase time, milliseconds |
| `warnings`, `redirect_chain`, `etag`, `last_modified`, `not_modified` | |
| `ocr` | `engine`, `model`, `device`, `pages`, `fallback` when OCR was used, else `None` |

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

AGPL-3.0-only, with a commercial license available for use without AGPL obligations. See the repository's [LICENSE](../../LICENSE) and [COMMERCIAL.md](../../docs/COMMERCIAL.md).
