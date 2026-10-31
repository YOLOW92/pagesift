# pagesift (Node.js)

0.20.0: see the [release and migration guide](https://github.com/YOLOW92/pagesift/blob/main/docs/releases/0.20.0.md). New collection/archive cores are available through Rust and CLI; this package keeps its documented scraping interface.

The npm package `pagesift`: pagesift's Rust engine for Node.js, built with [napi-rs](https://napi.rs).
Prebuilt binaries, so you don't need a Rust toolchain or a headless-browser download.
The API is designed for throughput first: batches are scheduled in Rust, results stay in Rust until you read them,
and CPU work never runs on the Node main thread unless you ask for the synchronous `convert`.

```js
import { readFile } from 'node:fs/promises'
import { convert, convertMany, Scraper, PagesiftError } from 'pagesift'

// HTML you already have -> Markdown (no network)
const page = convert(html, { url: 'https://example.com/post' })
console.log(page.markdown, page.metadata.title)

// Many documents: one call, bounded Rust workers, bytes in (no string transcoding)
const pages = await convertMany(await Promise.all(files.map((f) => readFile(f))))

// Fetching: create one Scraper and keep it (connection pool, robots.txt cache, rate-limit state, browsers)
const scraper = new Scraper()
try {
  const p = await scraper.scrape('https://example.com', { formats: ['markdown', 'links'] })
} catch (e) {
  if (e instanceof PagesiftError) console.log(e.code, e.details) // e.g. ROBOTS_DISALLOWED
}

// Results as they finish; `break` cancels the rest
for await (const { url, page, error } of scraper.scrapeStream(urls, { concurrency: 32 })) {
  if (error) console.log(url, error.code)
  else console.log(url, page.markdown.length)
}
await scraper.close()
```

## API

| | Returns | Notes |
|---|---|---|
| `convert(html, options?)` | `Page` | Synchronous, on the calling thread |
| `convertAsync(html, options?)` | `Promise<Page>` | On a Rust thread |
| `convertMany(htmls, options?)` | `Promise<Page[]>` | Whole batch in parallel in Rust; same order as input |
| `new Scraper(config?)` | | Keep one instance for the life of the process |
| `scraper.scrape(url, options?)` | `Promise<Page>` | Rejects with `PagesiftError` |
| `scraper.scrapeMany(urls, options?)` | `Promise<BatchItem[]>` | Same order as input; one failure only affects its own item |
| `scraper.scrapeStream(urls, options?)` | async iterable of `BatchItem` | Completion order; leaving the loop cancels pending work |
| `scraper.close()` | `Promise<void>` | Stops browser processes |

`html` is a `string` or UTF-8 bytes (`Uint8Array` / `Buffer`). Don't modify a buffer while a call that uses it is running.

**Options.** `formats` (`'markdown' | 'html' | 'rawHtml' | 'links' | 'images'`, default `['markdown']`; only the listed outputs are computed),
`onlyMainContent` (default `true`), `url` (convert only). Scraping adds `engine` (`auto` / `http` / `browser`), `timeout`, `waitFor`,
`headers`, `ignoreRobotsTxt`, `mobile`, `blockAds`, `ifNoneMatch`, `ifModifiedSince`; batches add `concurrency` (default 16).

**`Page`.** `markdown`, `html`, `rawHtml`, `links`, `images` (`null` when not requested) and `metadata`
(title, description, language, `statusCode`, `og*` keys, and `metadata.pagesift`: content hashes, redirect chain, ETag, engine used, timings).
Each property read copies that one field into JS; fields you never read are never copied. `toJSON()` returns everything as a plain object.

**`BatchItem`.** `{ index, url, page?, error? }`, with exactly one of `page` or `error`.
`error` is `{ code, message, details?, retryAfter? }`, a plain object rather than an `Error`, so a big batch doesn't collect stack traces.

**Errors.** Single calls throw or reject with `PagesiftError` (`code`, `details`, `retryAfter`). The codes match the pagesift HTTP API:
`ROBOTS_DISALLOWED`, `ACCESS_BARRIER`, `RATE_LIMITED`, `HOST_COOLDOWN`, `TIMEOUT`, `SSRF_BLOCKED`, `TOO_LARGE`,
`UPSTREAM_ERROR`, `NETWORK_ERROR`, `DNS_FAILURE`, `BUSY`, `UNSUPPORTED_OPTION`, `INVALID_REQUEST`, `INTERNAL_ERROR`.
pagesift never tries to get around a block: an access barrier is reported with `details` (vendor, kind, confidence), not bypassed.

**`Scraper` config.** `userAgent`, `allowPrivateNetwork` (off by default), `requestIntervalMs` (per host, default 1000),
`perHostConcurrency` (default 2), `maxBrowsers`.

## Why it's shaped this way: benchmarks

Measured on 2026-10-21 on linux-x64 with 16 threads, Node v24.21.0, release build. Other jobs were running on the machine
(load average 2–6), so take the absolute numbers as rough; the ratios are what matter.
Reproduce with `bench/bench.mjs` (see Development).

### Against Readability + Turndown

Same 597 pages (the frozen pagesift evaluation set, 179 MB of HTML). Readability needs a DOM, so JSDOM parsing is included in its time.

| Metric | pagesift | Readability + Turndown | |
|---|---|---|---|
| Per-page time p50, ms (lower is better) | 2.34 | 121.80 | 52× faster |
| Per-page time p95, ms (lower is better) | 11.86 | 716.18 | 60× faster |
| Per-page time mean, ms (lower is better) | 4.20 | 252.15 | 60× faster |
| Throughput, pages/s (higher is better; pagesift `convertMany` with Buffer input, Readability is single-threaded) | 1284 | 4.0 | 324× |
| Peak process RSS, MB (lower is better) | 1248 | 4455 | 72% lower |

### Batching and zero-copy input (same 597 pages)

| Approach | Throughput, pages/s (higher is better) | vs sync | Main-thread utilisation (lower is better) |
|---|---|---|---|
| `convert` in a loop | 238 | 1× | 100% |
| `convertAsync` × 597 + `Promise.all` | 857 | 3.6× | 55% |
| `convertMany` with strings | 874 | 3.7× | 65% |
| `convertMany` with Buffers | **1284** | **5.4×** | **17%** |
| `convertMany` with Buffers, all four formats (for comparison) | 800 | 3.4× | — |

Turning a JS string into Rust UTF-8 has to happen on the main thread, and for 179 MB that's most of the remaining main-thread work.
With bytes there's nothing to transcode. `convertMany` with strings runs that whole transcoding inside one call, so for very large
string batches either pass Buffers or split the batch.
Computing only the formats you ask for adds another 1.6× (1284 vs 800 pages/s).

### Lazy fields (one batch of 597 `Page`s with markdown, html, links and images computed)

| Read | Main-thread time, ms (lower is better) |
|---|---|
| `page.markdown` only | 13.81 |
| `page.toJSON()` (everything) | 203.17 |

Fields stay in Rust until read, so reading only what you need is 15× cheaper on the main thread.

### Scraping (local HTTP server in a child process, 16 hosts, ~3 KB pages, so fixed per-request costs dominate)

| Design | Measurement | Result |
|---|---|---|
| Reuse one `Scraper` | 100 sequential pages, one Scraper, ms (lower is better) | 34 |
| | 100 sequential pages, new Scraper per page, ms (lower is better) | 289 (8.6× slower) |
| Batches scheduled in Rust | `scrapeMany`, 2000 pages, pages/s (higher is better) | 13864 |
| | `scrape` × 2000 + `Promise.all`, pages/s (higher is better) | 14329 |
| | Main-thread utilisation during `scrapeMany` (lower is better) | **2%** |
| | Main-thread utilisation during `scrape` + `Promise.all` (lower is better) | 51% |
| Completion order | `scrapeMany`: first result, one 1 s page among 99 fast ones, ms (lower is better) | 1002 |
| | `scrapeStream`: first result, ms (lower is better) | **21** |
| | `scrapeStream`: all 99 fast pages, ms (lower is better) | 157 |

A new `Scraper` per page pays for a fresh connection and a robots.txt fetch every time.
`scrapeMany` and individual `scrape` calls reach the same throughput, but the batch leaves the main thread almost idle (2% vs 51%),
which matters when the same process also serves requests. `scrapeStream` delivers results as soon as each page finishes instead of waiting for the slowest one.

Known limit: requests to a single host currently top out at about 1000 per second even with `requestIntervalMs: 0`
(the rate limiter waits for a 1 ms timer tick per request), which is why this benchmark spreads its pages over 16 hosts.

## CPU budget

Asynchronous conversion shares at most `max(1, logical CPUs - 1)` Rust worker slots across calls,
leaving capacity for Node's event loop. `convertMany` validates and copies all inputs before starting
CPU workers, so parser work does not compete with the main thread while it copies strings.
Large string batches still copy synchronously; use UTF-8 buffers or smaller batches when needed.
The timing figures above predate this 0.20.0 scheduling change; rerun them on your deployment hardware.

## Platforms

| Target | Status |
|---|---|
| linux-x64-gnu, linux-arm64-gnu | native build in a glibc 2.28 container (requires glibc 2.28 or newer, checked at release) and full binding tests in Linux CI |
| darwin-x64, darwin-arm64 | native build and full binding tests in macOS CI |
| linux-x64-musl | native musl build and full binding tests on Alpine |
| win32-x64-msvc | native MSVC build and full binding tests on Windows |

Node 18 or newer is required. Release CI runs the full binding tests on Node 18, 20, 22, 24 and 26
(Linux x64 on glibc 2.28; the other platforms on 18 and 26). `npm install pagesift` selects the matching optional native package;
Linux musl and glibc packages are separate. Windows supports conversion and HTTP scraping;
local browser process management is unavailable. The Rust CDP client can connect to an existing remote endpoint.

## Development

```bash
npm install
npm run build        # release build + index.js / index.d.ts
npm test             # node --test, local HTTP server only
```

Benchmarks, from the repository root:

```bash
node crates/pagesift-node/bench/bench.mjs extract benchmarks/data/inputs/manifest.jsonl <dir with jsdom, @mozilla/readability, turndown in node_modules>
node crates/pagesift-node/bench/bench.mjs scrape
```

`index.js` and `index.d.ts` are generated by `napi build` (types from `src/`, plus `header.d.ts`) and committed.
`scripts/postbuild.mjs` adds the two exports the generated loader can't see (`PagesiftError`, `version`).

This crate doesn't use `#![forbid(unsafe_code)]`, because the napi-rs macros generate unsafe code. Our own code here contains no `unsafe`.

## License

AGPL-3.0-only, with a commercial license available for use without AGPL obligations. See the repository's [LICENSE](../../LICENSE) and [COMMERCIAL.md](../../docs/COMMERCIAL.md).
