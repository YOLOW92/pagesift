# PageSift 0.73.0 Release Notes: All Cores Again, Hardened Inputs, Fewer False Positives

> **Release Version**: `v0.73.0`
> **Release Date**: 2026-10-26
> **License**: AGPL-3.0-only (Commercial licensing available)

---

## Overview

0.73.0 is a fix release driven by a three-way code review (correctness, architecture, release
process) and about 150 adversarial input tests: broken encodings, pathological HTML, malformed
HTTP, redirect and SSRF tricks, hostile feeds, crawl traps and API abuse.

- **Content extraction uses every core again.** Since 0.70.0, `scrape`, `crawl` and `serve`
  extracted pages on a single core. The API server now serves 5–7× more requests per second.
- Crashes, data corruption and denial-of-service paths found by the review are closed.
- Several 0.72.8 heuristics that misfired on real pages are narrowed — for example, short
  Substack posts were reported as age-verification barriers.
- The API server enforces timeouts against slow clients and upper bounds on every parameter.

All numbers below were measured on the same machine (i7-10700, 8 cores / 16 threads, 15 GB),
with 0.72.8 and 0.73.0 run alternately under identical conditions.

---

## Extraction runs on all cores again

`CPU_SLOTS`, the semaphore that bounds concurrent extraction, sized itself with
`std::thread::available_parallelism()`. On Linux that returns the **calling thread's** CPU
affinity. The semaphore is initialized lazily on a tokio worker, and since 0.70.0 the CLI pins
every runtime thread to a single CPU — so the semaphore always had exactly one permit.

0.73.0 sets the capacity from the cgroup-aware hardware topology on the main thread before any
thread is pinned, and pins only the runtime workers; blocking-pool threads (extraction, PDF
parsing) are left to the scheduler.

| `serve`, `/v1/scrape`, 16 concurrent clients | 0.72.8 | 0.73.0 |
| :--- | ---: | ---: |
| Requests per second (higher is better) | 309–336 | 1,153–2,196 |
| p50 latency (lower is better) | 36–45 ms | 4.5–6.1 ms |
| Peak server RSS (lower is better) | 53–56 MB | 89–92 MB |

The higher memory is the cost of running 16 extractions at once instead of one. The figures
above used the `auto` engine, which sends some pages to a browser for 1–7 s each. With
`engine: http` and the default global concurrency of 64, 16/32/64 clients reach 2,373/2,540/
2,458 requests per second — about 88% of what in-process extraction alone achieves on this
machine — so the server itself is not the bottleneck.

**Memory allocator.** The release binary keeps glibc malloc limited to 4 arenas with a 256 KB
mmap threshold (pure Rust, no bundled allocator). On this 16-thread machine that was the fastest
setting tested: 16 arenas, glibc defaults and jemalloc were all slower or used 2–3× the memory.
It has **not** been verified on large multi-socket servers, where a fixed 4 arenas may become a
lock bottleneck; the 0.70.0 figures from a 192-vCPU server were measured with a benchmark runner
built with mimalloc, not with the release binary.

---

## Crashes, corruption and denial of service

| Problem | 0.72.8 | 0.73.0 |
| :--- | :--- | :--- |
| CJK page whose byte 4096 falls inside a character | panic, `INTERNAL_ERROR` | extracted |
| UTF-8 page with a few stray Latin-1 bytes | whole page re-decoded as Windows-1252 | UTF-8 kept, stray bytes become U+FFFD |
| PDF served as `application/octet-stream` | rejected as binary | parsed as PDF |
| One element with 40,000 attributes | 3.00 s | 0.02 s |
| Client sending one header byte every 5 s | connection held indefinitely | closed after the 30 s header timeout |
| RSS item `<link>javascript:…</link>` | emitted as a Markdown link | dropped |
| Lightpanda + `--allow-cidr`, subresource `[::ffff:127.0.0.1]` | not in the block list | blocked |

---

## Heuristics that misfired on real pages

- **Age gates.** 0.72.8 treated `age-gate`/`age_gate` in the raw HTML as proof of an
  age-verification page when the page had little visible text. Substack pages carry
  `"test_age_gate_user":false` in their inline JSON, and `language-gate` matched too. The rule
  only fired when the marker fell in the scanned part of the HTML and the page showed under
  2,000 characters, so long articles were unaffected: on 66 real Substack pages, 0.72.8 rejected
  2 short "hidden open thread" posts with `ACCESS_BARRIER` and 0.73.0 rejects none. Age gates
  are now recognized from visible confirmation text only. Steam's age check is covered by a
  local test; Steam currently shows no age check to anonymous visitors from our test location,
  so it could not be re-checked live.
- **Author lists** are matched by whole class/id tokens (`authors`, `author-list`,
  `ltx_authors`…) and only exempt from link-density pruning. Sidebar "our writers" widgets and
  footer author boxes are pruned again; arXiv keeps its author line.
- **`v-cloak` / `x-cloak`** on a `<body>` child holding more than 1,000 characters is treated as
  a hydrating app root, not a hidden template.
- **`<template>`** content can no longer be selected as the article body.
- **`excerpt`** is no longer a negative class. `teaser` stays: removing it put product-page
  review previews back into the output (WCXB product −0.009).
- **`openai_key`** requires a real key shape; on 500 real pages it went from 12 false positives
  (CSS variables such as `sk-line-height--extralarge`) to 0.
- **VideoObject fallback** only replaces a body that is essentially empty (under 30 characters).

---

## Protocol handling

- `Content-Encoding: gzip, gzip` is decoded in reverse order, up to three layers, each layer
  bounded by the body size limit.
- A body labelled gzip/zstd without the format's magic bytes is used as-is with a
  `content_encoding_mismatch` warning; a corrupt stream reports a decoding error rather than
  "exceeded the size limit".
- A `Location` header with raw UTF-8 (or Latin-1) bytes is percent-encoded and followed; SSRF
  checks and the redirect limit apply as before.
- `application/json` responses become a formatted ```json code block.
- A final 3xx without a usable `Location` warns `http_redirect_status`; a 304 to an
  unconditional request warns `not_modified`; `pagesift scrape` exits 2 for both when the body
  is empty.
- With a proxy configured, hostnames are resolved locally only when `allow_cidrs`/`allow_ips`
  are set, so `socks5h` users no longer leak DNS. A host resolving inside an allowed range still
  connects directly; a failed lookup (2 s cap) falls back to the proxy.

---

## Crawl scope

Where should a crawl stay when you start it at `https://example.com/blog/post-3`? Earlier
versions picked one fixed rule and were each wrong half the time: 0.70 crawled one page,
0.71–0.72.8 always went up a directory (so `/docs/python` also crawled `/docs/java`). 0.73.0
decides from what the start page links to.

1. Start in the narrowest directory: the seed's own path, or the directory of a file-name seed.
2. After fetching the start page, count its links below that directory (children). If the page
   has body text (≥ 200 letters or digits outside links) and fewer than 3 children, it is a leaf
   page: walk up from the parent directory and widen to the nearest one the page links at least
   3 sibling pages in. Never widen to the site root. Walking up matters for date-based URLs —
   `/2024/Dec/31/post/` has no siblings on the same day but many in `/2024/Dec/`. A trailing
   slash does not make a seed a directory; directory pages are recognized by their children.
3. Say so: `warning: crawl_scope: widened path scope from /blog/post-3/ to /blog/ (…)`.
   Sitemap URLs held back by the narrow scope are queued after widening.

| Seed | 0.70 | 0.71–0.72.8 | 0.73.0 `auto` |
| :--- | :--- | :--- | :--- |
| `/docs/python` (has child pages) | `/docs/python/*` | `/docs/*` | `/docs/python/*` |
| `/docs/index.html` | nothing followed | `/docs/*` | `/docs/*` |
| `/blog/post-3` (article, links other posts) | 1 page | `/blog/*` | `/blog/*`, with warning |
| `/shop/c/socks` (empty category, links only) | 1 page | `/shop/c/*` | 1 page |
| `/about` (directly under the root) | 1 page | 1 page | 1 page |

`--scope exact` never widens, `--scope parent` starts in the page's section — the parent
directory, skipping year/month/day directories (`/blog/2026/07/21/post/` → `/blog/`), or the
whole host when that section is the root — and `--scope domain` (or `--entire-domain`) crawls
the host. The API takes the same values in `scope`;
`crawlEntireDomain: true` still means `domain`.

Measured on local test sites (higher is better). The rules were written against the first set;
the two holdout sets were written afterwards with their expected results fixed in advance, and
no threshold was changed after seeing them (only the planned body-text check was added after
holdout 1 found an empty category page widening):

| Set | 0.72.8 precision / recall | 0.73.0 before this change | 0.73.0 |
| :--- | :--- | :--- | :--- |
| Development, 7 cases | 0.804 / 1.000 | 1.000 / 0.631 | 1.000 / 1.000 |
| Holdout 1, 5 cases | 0.810 / 0.971 | 1.000 / 0.314 | 1.000 / 0.821 |
| Holdout 2, 4 cases | 0.577 / 1.000 | 1.000 / 0.564 | 1.000 / 1.000 |

On the real web (limit 15 pages, `engine: http`, robots.txt honoured; recall is measured against
a whole-host crawl restricted to the intended section with the same limit). Batch 1 was run
before the ancestor walk was added; batch 2 used sites not seen before, with intended sections
written down before the run:

| Real sites (precision / recall) | 0.72.8 | 0.73.0 before this change | 0.73.0 |
| :--- | :--- | :--- | :--- |
| Batch 1: Python, Rust book/std, Go, MDN, React, GitHub Docs, Kubernetes, BBC, three blogs, Substack (14) | 0.983 / 0.863 | 1.000 / 0.547 | 0.983 / 0.929 |
| Batch 2: Django ×2, FastAPI, Docker, Node, PostgreSQL, Fowler, Go blog, Ars Technica, Joel on Software (10) | 0.993 / 0.543 | 1.000 / 0.457 | 0.993 / 0.820 |

Batch 2 excludes LWN, which returned no pages to any version. Pages the start page links to
outside the intended section (precision below 1) are mostly one stray link per crawl, such as
Go's `/doc/` linking `/doc/devel/`.

Known misses, where the start page itself carries no evidence:
- Posts that link only their previous and next post (jvns.ca, joelonsoftware.com) stay narrow.
  `--scope parent` crawls the section (`/blog/`) or, for root-level date URLs, the site.
- News articles whose related links all point to other sections (Ars Technica `/gadgets/…`
  linking `/science/…`) stay narrow; widening to the whole site needs `--entire-domain`.
- When nothing is followed, the `crawl_followed_zero` warning now suggests `--scope parent`.

---

## API limits and error format

| Parameter | Limit |
| :--- | :--- |
| scrape `timeout` | 300,000 ms |
| crawl `limit` | 100,000 |
| crawl `maxDepth` / `maxDiscoveryDepth` | 100 |
| map `limit` | 100,000 |
| URLs per batch scrape / extract | 10,000 |
| URL length | 8 KB |
| request headers / request body | 30 s each |
| whole synchronous request | 360 s |

`extract.schema` must be a JSON object. Query-string and path-parameter errors, 405 and 415 now
return the standard `{"success": false, "code": …, "error": …}` body.

---

## Quality and performance

Extraction quality on three labeled sets (all higher is better):

| Set | Metric | 0.72.8 | 0.73.0 |
| :--- | :--- | ---: | ---: |
| AEB (181 news pages) | F1 | 0.958 | 0.958 |
| Firecrawl (597 pages) | content score | 0.680 | 0.682 |
| WCXB (1,869 pages) | F1 | 0.8522 | 0.8533 |
| WCXB listing | F1 | 0.6925 | 0.7065 |
| WCXB product | F1 | 0.6467 | 0.6467 |

Page-by-page across all 2,647 pages: no crashes, no page emptied, two pages shrank by more than
30% (one lost a run of decorative asterisks; one tag page lost its heading together with its
boilerplate).

Speed, alternating old/new runs (lower is better):

| Benchmark | 0.72.8 | 0.73.0 | Change |
| :--- | ---: | ---: | ---: |
| Extraction, 2,786 real pages, 1 thread | 6.405 s | 6.198 s | −3.2% |
| Extraction, 2,786 real pages, 16 threads | 1.034 s | 0.979 s | −5.3% |
| Synthetic blog page 100 KB (`perf_gate`) | 3.73 ms | 3.78 ms | +1.3% |
| Synthetic wiki page 250 KB (`perf_gate`) | 16.31 ms | 16.61 ms | +1.8% |
| Charset detection, 3,192 files | 2.043 s | 2.004 s | −1.9% |
| `crawl` 1,000 pages, one host | 1.96–2.02 s | 1.95–2.00 s | ±0 |

Binary size: 38,478,112 → 38,657,584 bytes (+0.47%).

---

## Re-checked on the live web

Each fix that has a public example was re-run against real sites with 0.72.8 and 0.73.0
(`engine: http`, robots.txt honoured):

| Check | 0.72.8 | 0.73.0 |
| :--- | :--- | :--- |
| PDF served by raw.githubusercontent.com as `application/octet-stream` | refused (`UNSUPPORTED_OPTION`) | 94,696 characters |
| `api.github.com` JSON | plain text | ```json code block |
| `httpbin.org/status/304` | exit 0, empty, no warning | exit 2, `not_modified` |
| `httpbin.org/status/300` (no `Location`) | exit 0, `empty_content` | exit 2, `http_redirect_status` |
| 66 Substack pages | 2 rejected as age gates | 0 |
| jvns.ca Atom feed with `[DEST]` in code | unescaped brackets | `\[DEST\]` |
| Chinese Atom feed, GBK and Shift_JIS pages, arXiv authors, YouTube | correct | identical |

A broader pass over 45 real pages (news, docs, shops, forums, Chinese/Japanese/Korean portals,
feeds, a PDF) changed nothing except the fixes above, plus one Wikipedia paragraph with a code
sample that 0.72.8 dropped and 0.73.0 keeps. Six pages fail identically in both versions, all
correctly reported: four disallow crawlers in robots.txt, two sit behind DataDome or Cloudflare
challenges.

The single-core fix does not show in polite crawls of real sites: crawling 150 Rust `std` pages
takes about 1.1 s in both versions and uses about 1.2 s of CPU, so the network is the limit. It
matters when extraction is the bottleneck, as in a `serve` instance under concurrent load.

---

## Upgrade notes

- Crawls from a directory-like seed with its own child pages (`/docs/python`) no longer reach
  sibling directories as in 0.71–0.72.8; use `--scope parent`, `--entire-domain` or `--include`.
- Proxy users who reach intranet hostnames directly should list them in `NO_PROXY` or keep
  `--allow-cidr` set so they are resolved locally.
- Scripts checking `pagesift scrape`'s exit code now see 2 for empty 3xx/304 results.
- API clients sending values above the new limits receive HTTP 400 with the limit in the message.
