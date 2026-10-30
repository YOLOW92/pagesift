# Changelog

All notable changes are listed here. The project follows [Semantic Versioning](https://semver.org/).

## [0.81.4] — 2026-10-31

Dependency security update. See [the release notes](docs/releases/0.81.4.md).

### Security
- tokio 1.54.0 → 1.54.1 (RUSTSEC-2026-0384), zstd 0.13.3 → 0.14.3 (RUSTSEC-2026-0391) and
  zstd-safe 7.3.0 → 8.2.0 (RUSTSEC-2026-0398, RUSTSEC-2026-0403). Crates that pinned
  tokio 1.54.0 now require 1.54.1 or newer.

### Fixed
- `cargo deny` license exceptions now list `pagesift-fuzz`, `pagesift-office` and
  `pagesift-testkit`.

## [0.81.3] — 2026-10-30

Firecrawl's `includeTags` / `excludeTags` now work, and the Jina Reader routes sit behind the same
authentication, rate limits and page quotas as the Firecrawl routes. See
[the release notes](docs/releases/0.81.3.md).

### Added
- `includeTags` / `excludeTags` (HTTP API, MCP, Python `include_tags` / `exclude_tags`): CSS
  selectors (tag, `.class`, `#id`, …) applied before extraction. `excludeTags` drops matching
  elements with their subtrees; `includeTags` keeps only the outermost matches in the body, in
  document order, and keeps `<head>` so metadata is still extracted. `rawHtml` is unchanged. An
  invalid selector returns `INVALID_REQUEST` before anything is fetched; the Python SDK used to
  raise `UnsupportedOption` for these options.

### Security
- `GET /r/{url}` and `POST /r` (Jina Reader) were mounted outside the API-key middleware, so with
  `PAGESIFT_API_KEYS` set they could still be called without a key and did not count against
  per-minute limits or page quotas. They now require a key, count as scraping requests, consume
  one page each and are reported in `/metrics`.

### Documentation
- CONTRIBUTING, SECURITY and the bot page state the per-site (not per-IP) rate-limit rule for
  deployments with several egress proxies.

## [0.81.2] — 2026-10-30

Restore the content that 0.81.1 dropped on bare client-side redirect pages, without giving up the
adaptive browser-upgrade stop. For a redirect page with no HTTP content, the browser now captures
the target's DOM as soon as main-frame text appears, instead of waiting for `DOMContentLoaded` or
network idle. The wait is bounded relative to the page's HTTP time and the caller's timeout, not by
a fixed number of seconds. See [the release notes](docs/releases/0.81.2.md).

### Performance
- A2 replay, 269 pages: 0.81.0 110.7 s / 21.9 s (1/16 workers) → 0.81.2 88.0 s / 13.5 s.
- Per-page comparison against 0.81.0: zero pages lose content; total extracted characters
  2,376,184 → 2,376,358 (+174).
- The redirect page that lost 405 characters in 0.81.1 now returns 476 characters in 9.4 s
  (0.81.0: 405 characters in 17.0 s).

## [0.81.1] — 2026-10-29

`engine: auto` browser upgrades are now self-limiting. The HTTP document is already a valid
result; a browser upgrade is stopped when the extracted main content stops improving, using
running distributions of navigation, settle and first-content times instead of fixed seconds.
Pages whose HTTP extraction was empty keep the full render time, and explicit
`engine: browser` is unchanged. See [the release notes](docs/releases/0.81.1.md).

### Performance
- A2 replay, 269 pages: 1 worker 119.2 s → 77.3 s (2.3 → 3.5 pages/s); 16 workers 28.4 s → 10.5 s
  (9.5 → 25.7 pages/s), with 301 fewer extracted characters over the whole set.
- Browser upgrades that reproduce the HTTP text now return the HTTP document, keeping the smaller
  memory footprint and the original timings.

### Changed
- Added `render_no_gain_http_fallback`, `render_timeout_http_fallback` and
  `render_failed_http_fallback` warnings for `engine: auto` upgrades that fall back to HTTP.
- A bare client-side redirect with no HTTP content is sent directly to a full browser; Lightpanda
  is skipped for that page.

## [0.81.0] — 2026-10-29

Page relevance for focused crawls is rebuilt around the facets of the query, measured against
pages judged by reading their content; the whole pipeline is made faster by working with the
hardware (contiguous data, fewer allocations, a small-block allocator cache aware of NUMA nodes,
whole-program LTO, profile-guided optimisation of the release binaries). See
[the release notes](docs/releases/0.81.0.md).

### Changed
- Focused crawl: page relevance scores how much a page is *about* each facet of the query instead
  of how many query words it contains. The units are the query words plus adjacent query words
  that occur together on the site far more often than chance (`computer science`,
  `student housing`); a unit counts more in the title, the URL or a heading than in the body, the
  body term frequency is normalised by the site's mean page length, and each unit is weighted by
  its inverse document frequency on the site. Everything is estimated from the pages the crawl has
  fetched. On 1,833 pages judged by content (61 universities, five directions, half of the
  universities held out), separating useful from useless pages: AUC 0.79 → 0.86. The adaptive stop
  thresholds were converted on the same pages (0.12 → 0.05, 0.3 → 0.15; same share of useful pages
  kept). Relevance values in `index.jsonl` and the crawl output are on the new scale.
- Focused crawl: a subtree whose harvest rate is clearly below the rest of the crawl (95% Wilson
  intervals, successive elimination) is no longer scheduled; a cluster that has proven productive
  is exempt from the per-cluster cap. Blind links (no sign of the query) are probed one at a time,
  and once the query is covered with only blind links left, the crawl stops.
- English query on an English start page: `english.<site>` (an English department) is no longer
  taken for the English version of the site.
- The start page of a crawl is retried up to four times (2, 4, 8, 16 s) on timeouts, network
  errors and 5xx; other pages keep two retries.

### Performance
- Release builds use fat LTO with one codegen unit (CLI binary 42 MB → 25 MB) and, on Linux x86-64,
  Linux arm64 and macOS arm64, profile-guided optimisation trained on generated pages and the
  repository's document fixtures (falls back to a plain build if any PGO step fails).
- The CLI uses its own small-block allocator cache: allocations up to 512 bytes come from
  per-thread free lists, refilled in batches from global pools sharded by NUMA node; each thread
  keeps at most its share of the L2 cache (read from sysfs).
- PDF: glyph text is an inline short string instead of two heap allocations per glyph.
- HTML main-content extraction: the `<title>` comparison is linear instead of quadratic and its
  signature is built once per page; attribute and date scans skip ahead bytewise.
- CSV: the table is stored contiguously and fields, escaping and cell cleanup work on byte runs.
- xlsx: number formats are compiled once per code; attributes are unescaped only when matched.
- Output of every converter is byte-identical to 0.80.10 on the benchmark corpora (394 web pages,
  271 PDFs, 377 Office/CSV files).

### Fixed
- Crawl: every failed attempt on a non-start page counts against its host at once; when the host is
  dropped, its pages waiting for a retry are reported instead of retried (a dead host used to take
  six timeouts to stop, now two).

## [0.80.10] — 2026-10-28

The page limit of a focused crawl becomes adaptive, and the discovery problems from the
fourteenth round of outside testing are fixed; three extraction problems from the AiSeek report
(teasers instead of the body, sidebars kept, relative links in uploads) are fixed too. See
[the release notes](docs/releases/0.80.10.md).

### Added
- `crawl --query … --adaptive` without `--limit`: the crawl decides how many pages it needs and
  `--limit` is only a cap (500 when not given; without `--adaptive` the default stays 100). The
  HTTP API's `limit` already was a cap.
- Adaptive stop reason `no_relevant_pages`: nothing relevant has been found and `2 × patience`
  pages in a row were reached only through links without any sign of the query (their score is
  the section prior). Such sites used to be crawled up to the limit.
- `POST /v2/parse` takes an optional `url` field: the address the uploaded HTML came from.
  Relative links resolve against it (a `<base href>` in the page still wins) and it becomes
  `metadata.sourceURL`. It is never fetched.

### Changed
- Adaptive crawl: reaching the coverage threshold no longer stops the crawl at once. While pages
  keep adding new relevant content the crawl goes on; it stops after `ceil(patience / 2)` pages in
  a row that add nothing new, and only when the best queued link carries a sign of the query.
  Coverage also needs evidence from at least two pages (one overview page can mention every term
  often enough).
- Focused crawl, English query: once the site's English content is known (the start page is
  English, or the English home page was found), links on pages in other languages no longer get
  the section prior. German and Norwegian main sites used to take most of the budget.
  `/english/research/` counts as a first-level section of the English site, like `/en/research/`.
- Focused crawl: a short section named after a query word under an about/news section
  (`/about/housing/`) is content, not furniture.
- Article extraction: when the `<article>` that holds the headline is only a teaser whose text
  reappears in a longer body elsewhere on the page, that body is extracted (press-release
  templates). In two-column layouts without an article marker, the region is narrowed to the
  column of the headline that matches `<title>`; story cards (a link and a summary ending in an
  ellipsis) do not count as body text. A section name in the only `<h1>` is no longer taken for
  the headline when an `<h2>` matches `<title>`.

### Fixed
- `/v2/parse` and `scrape <file>` of HTML now give the same completeness warnings as fetched
  pages (`partial_content` and the others).

## [0.80.9] — 2026-10-28

The last 0.80 release (0.80.6–0.80.8 were not released; everything since 0.80.5 is here):
engine discovery and `doctor` from the thirteenth round of outside testing, a wider focused-crawl
check on 69 universities in five directions, work on the weak spots of the mixed benchmark
(hard PDFs, one-process-per-file start-up, main-content recall), and four fixes from code review.
Instruction counts stay within 2% of 0.80.0. See [the release notes](docs/releases/0.80.9.md).

### Added
- `crawl --ignore-robots` and `map --ignore-robots`. On the command line the flag is the
  permission: whoever runs the CLI can edit its config, so they are the operator. The HTTP API
  still needs `allow_crawl_ignore_robots = true` before a request may set `ignoreRobotsTxt`.
  Until now only single-page `scrape` had the flag, and crawls could ignore robots.txt only
  through the API.
- `doctor` starts every engine in the chain once instead of only checking that the file exists.
  An engine that is installed but crashes on launch is reported: `FAIL` when no engine starts,
  `WARN` when another engine covers for it (with what that means for rendering).

### Changed
- Focused crawl (`--query`): queued links are re-scored when the term weights change. Weights
  are learned from the pages fetched so far, and until now each link kept the score it got
  when it was found, so the first links from the homepage (scored before any weights existed)
  stayed ahead of better links found later.
- Focused crawl: a page whose text is identical to one already delivered (the same page under
  another URL, such as `?ref=` tracking variants) is not delivered and does not count against
  `limit`; once two variants prove a query parameter changes nothing, queued variants are
  rewritten and skipped. On a 62-university run, same-path duplicates fell from 38 to 7.

- `pagesift scrape` (one URL or file per process) starts lighter: a single-threaded runtime
  without CPU topology probing or thread pinning (pinning stays for crawl, map and serve, which
  own the machine), and the system root certificates are read once per process, on the first
  HTTPS connection, instead of once per HTTP client at start-up. A local file never reads them.
  CPU per call on a small PDF 6.3 → 4.3 ms; one-process-per-file throughput +23% with 1 worker
  and +44% with 4 workers on a 4-core machine.

### Fixed
- Main-content extraction keeps the headers of accordions and FAQ blocks: a `<button>` with
  `aria-expanded`/`aria-controls` and real text (the WAI-ARIA disclosure pattern, like
  `<summary>`) was dropped in article mode, where buttons are removed and only paragraph-like
  elements are rendered, so answers appeared without their questions. Short controls ("Expand
  all") are still dropped.
- PDF: text in annotation appearances is extracted, as viewers show it: filled-in form fields,
  FreeText notes, stamps, signature blocks. Fields without an appearance stream
  (`/NeedAppearances`) contribute their value. Only the text is taken (field borders are not
  table rules); hidden annotations, links, popups and push buttons are skipped. On the pdf.js
  test files of the mixed benchmark, 61 of 100 complete instead of 53 (same machine, before and after).
- PDF: Type3 fonts whose `FontMatrix` is not the usual 1/1000 (for example `[1 0 0 -1]` with a
  `0.12 Tf`) were sized at a fraction of a point and dropped as invisible; the size is now
  corrected from the glyph widths when it is off by more than 4×. Made-up glyph names in a
  Type3 font's `/Differences` (`/BT`, `/a12`) fall back to the base encoding for that code.
- Browser engines are also looked for outside `PATH`: `~/.local/bin`, `~/bin`, `/usr/local/bin`,
  `/usr/bin` and `/opt/homebrew/bin` (`sudo`, systemd and cron often start pagesift with a
  stripped `PATH`), Lightpanda's release file names (`lightpanda-x86_64-linux` …), and the newest
  Chromium in Playwright's browser cache (`$PLAYWRIGHT_BROWSERS_PATH` or the default cache).
- The Office/EPUB/CSV output budget (`max_output_bytes`) is now a hard limit. An EPUB chapter
  was converted and kept whole before the budget was checked, so one 100 MB chapter passed a
  16 MB limit; chapters are now cut to what is left (and their XHTML shortened before conversion).
  CSV counted cell text but not the table's separators and escapes. Every conversion's final
  Markdown is now cut at the limit, at a line boundary, with the `output_truncated` warning.
- Manually re-running the release workflow for an existing tag built whatever branch was
  selected in the form; every job now checks out the tag's own commit (and the Docker image's
  revision label names it).
- The MSRV check in `scripts/ci.sh` used Rust 1.88 while `Cargo.toml` declares 1.89, and was
  skipped when that toolchain was missing. It now reads `rust-version`, installs the toolchain if
  needed, and also runs in CI.
- `scripts/check_wall_clock.py` now also checks `#[cfg(test)]` code inside ordinary source files
  (inline `mod tests`, test-only helpers, out-of-line `mod tests;` files), ignoring comments and
  strings; `tokio::time::Instant` inside a `start_paused` test counts as the paused clock.

## [0.80.5] — 2026-10-28

Fixes from two more rounds of outside testing on 0.80.3 (there was no 0.80.4 release; everything
since 0.80.3 is here): data-risk warnings reach Firecrawl's `data.warning`, age gates and block pages
are recognised as barriers, Office/CSV/EPUB conversion has an output budget, PDF damage warnings
count correctly, and rendering waits for pages that fill in after a timer. Instruction counts stay
within 2% of 0.80.0. See [the release notes](docs/releases/0.80.5.md).

### Added
- `crawl --out` writes `summary.json` next to `index.jsonl`: `stopReason`, `coverage` (with
  `--adaptive`), `termEvidence` and `pages`, the same fields as the API job status. Until now the
  stop reason and coverage were only printed on stderr; `index.jsonl` never had a coverage field.
- `index.jsonl` rows and crawl JSON lines carry `warning` when the page has a data risk.

### Fixed
- Rendering waited only for the network to go quiet (100 ms), so a page that fills itself in from
  a timer, without a request, came back as its "loading…" shell (about 100 ms of waiting on every
  engine; moli only looked better because of incidental requests). When the network is quiet but
  the page has almost no text (under 200 characters), rendering looks again every 250 ms for up to
  1.5 s and stops as soon as text appears; pages that already have text are not delayed.
- When one browser engine renders only a shell and the engines after it fail, the shell is
  returned with a `render_incomplete` warning naming them, instead of silently.
- Command-line options with a fixed set of values (`--engine`, `--format`, `--sitemap`,
  `tune --profile`) reject anything else: `--engine potato` used to run as `auto` without a word.
- Word, Excel, PowerPoint, CSV and EPUB have an output budget like HTML (response size limit) and
  PDF (page limit): conversion stops once the text passes `Limits::max_output_bytes` (16 MB) and
  returns what it has with an `output_truncated` warning. A 408 KB DOCX that unpacks to 120 MB of
  XML gave 84 MB of Markdown with a 348 MB peak and no warning; now 17 MB, 171 MB peak, warned.
  One counter in the shared places (XML reader text, CSV rows, EPUB chapters), no per-format code.
  An EPUB spine that lists the same chapter many times converts it once.
- PDF damage warnings: `content_missing` and `fonts_missing` count against the document's page
  count, not the pages recovered; a page that lost only some of its content streams says so
  (`1 (1 of 2 streams)`, e.g. arXiv's watermark stream at the end of the file). When the page tree
  is lost and nothing in the file states the page count, `pages_missing` says the total is unknown
  instead of staying silent (a 15-page paper cut to 15% came back as 13 pages with no warning).
- A body that is plainly HTML is handled as a web page even when the server labels it
  `application/pdf` (CDN or WAF block pages), so barrier detection applies; `%PDF-` bodies stay PDF
  whatever the label.
- Warnings that mean the result is incomplete or wrong (`content_missing`, `pages_missing`,
  `fonts_missing`, `thin_content`, `partial_content`, `empty_content`, `max_pages`, scanned PDF,
  decoding errors, HTTP error status, skipped rendering …) now fill Firecrawl's `data.warning`
  (and the response's top-level `warning`) for scrape, crawl pages and `/v2/parse`. They were only
  in `metadata.pagesift.warnings` and on the CLI's stderr, so pipelines reading the Firecrawl
  field saw nothing. One list (`pagesift::notice`) drives both `data.warning` and stderr.
- Age gates inside a large site frame are classified as an access barrier (`interstitial`,
  `generic.age_gate`): Steam's `/agecheck/` page was returned as a 547-character "page" with exit
  code 0, because the menus and footer pushed the page past the size where barrier phrases count.
  When the extracted main content is short, made of short lines, and holds a barrier phrase, it
  is classified on the main content alone; a short article that mentions age checks is not.
  More age-gate phrases (EN/DE/FR/ES/ZH/JA/KO). HTML benchmark: +0.19% instructions against 0.80.0.
- `pagesift-cdp` virtual scroll (`RenderOptions::virtual_scroll`) lost whole screens of rows
  when the browser was slow to paint: it collected after a fixed wait, so on a busy machine it
  read the old rows and the next scrolls merged into one `scroll` event. Each step now waits for
  its own `scroll` event, then for the DOM to settle (`wait_after_scroll` counts from the event).
  Steps scroll with `behavior: instant`, so pages with `scroll-behavior: smooth` no longer wait out
  the animation (3.0 s → 0.8 s on a 100-row list).

### Docs
- `SECURITY.md` listed 0.20.x as the supported line since 0.20; it now lists 0.80.x.
  `scripts/check_release.py` fails when the table does not name the current release line, so a
  version bump can no longer leave it behind.

### Tests
- Complexity guards (parsers, PDF, content extraction, robots matching, checkpoint recovery)
  measure the CPU time of the calling thread instead of wall time, through the unpublished
  `pagesift-testkit` crate; other tests competing for the CPU no longer trip them.
- The per-origin connection controller keeps all its times on tokio's clock and its tests run on
  a paused clock, so they are exact rather than timing-dependent.
- Wall-clock bounds that remain (browser readiness, slow-image, redirect, warm-origin threshold)
  sit just under the time the bug would take instead of close to the expected time; the
  failed-engine test counts launches instead of timing the second call.

## [0.80.3] — 2026-10-28

The focused crawl checked and fixed on 38 real university sites (0.80.2 had only synthetic ones),
and the D-2 follow-up found on the real arXiv paper. On the 32 sites both versions reached, useful
pages went from 29.6% (0.80.0) to 66.1%, international-student pages from 6.3% to 38.9%. See
[the release notes](docs/releases/0.80.3.md) and `benchmarks/history/0.80.3/`.

### Added
- PDF warnings `content_missing` (pages of a repaired file whose content streams are gone, named
  by page) and `fonts_missing` (pages whose font definitions are gone). The audit log counts
  `content_missing` as suspicious; the CLI prints it.
- Query crawls follow the start page's `<link rel="alternate" hreflang="en">` for English queries.
- pagesift-content: `Metadata::alternates` (hreflang alternates).

### Changed
- Query crawls default to the whole site (`scope: domain`); `scope` is now optional and crawls
  without a query keep `auto`. CLI `--scope` has no fixed default any more.
- Query crawls: failed fetches (errors, error statuses, challenges, robots-disallowed) are refunded
  to the page budget, up to `limit` refunds; hosts that time out twice or answer a challenge before
  delivering any page are dropped; two URLs that land on the same page deliver it once.
- Focused scoring: English home pages ending in `.html` and English subdomains are English entries;
  department subdomains' `/en/` are not; `/en/<section>/` counts as a section; home pages of all
  hosts share one cluster; the list discount counts only main-content links; hubs are judged by
  text outside link labels; adaptive relevance thresholds set from real pages (irrelevant below
  0.12, evidence from 0.3); more furniture (login/SSO/portal/LMS hosts and paths, shops, imprint,
  data protection, cookies); English queries demote other-language paths.
- Charset detection (HTML without a declared charset, CSV) feeds chardetng in 4 KiB blocks and
  stops once the guess has been stable for three blocks after 16 KiB: Shift_JIS CSV −6.4% and GBK
  CSV +1.0% instructions against 0.80.0 (the 0.80.1 decoding fix had cost +2.5% and +23%).
- Fetching: a same-host redirect from https to http is followed over https unless that would loop;
  query crawls fetch same-site `http://` links over https. `internal_*` and `elq*` query
  parameters are stripped as tracking parameters.

### Fixed
- A truncated PDF that kept every page dictionary but lost a page's content stream (the real arXiv
  1706.03762 cut at 50–80% loses page 1) reported all pages and no loss.
- A query crawl stopped after three pages when one page of the main host was behind a firewall
  rule (NTU), and spent its budget on timeouts and challenges (Manchester).

## [0.80.2] — 2026-10-28

The focused crawl (`query`) rewritten after a tester ran it over 159 university sites and got
25.6% useful pages; two PDF follow-ups from the 0.80.1 review; two flaky macOS tests. All crawl
changes are verified on synthetic sites only (the development container cannot reach the real
ones). See [the release notes](docs/releases/0.80.2.md).

### Added
- Crawl: `attachments` (CLI `--attachments`) fetches PDFs and Office documents linked from
  relevant pages with their own budget (`attachmentLimit`, `--attachment-limit`), outside the
  path scope, through the existing PDF/Office pipeline; needs `query`.
- Crawl: `metadata.pagesift.crawl` on every result (`depth`, `parent`, `score`, `attachmentOf`);
  `index.jsonl` gains `relevance`, `depth`, `parent`, `score`, `attachmentOf` and `status`.
- MCP: `pagesift_crawl` accepts `query`, `adaptive`, `attachments`, `attachmentLimit`.
- pagesift-api: `CrawlTrace`; pagesift-content: `site_key` (registrable domain).
- `focus_gate` example (8 synthetic campus sites plus a SaaS developer portal and a city
  services site with their own queries) runs from `scripts/ci.sh` with a precision floor.

### Changed
- Focused crawl: links are scored from their anchor text whether or not they sit in the main
  content (navigation links included), the words in the URL (host labels, path, query string) and
  the line around them; query terms are weighted by document frequency across the crawled pages;
  news/press/changelog/events/about/legal paths and short furniture anchors are demoted (×0.2,
  never for a word the query asks for), dated leaves (`/2026/05/…`) ×0.1, hub pages and article
  lists (little text outside link labels) halved, long same-section link lists discounted;
  top-level section links (`/docs/`, `/services/`) get a floor score so they are explored before
  dated news; the queue alternates between sections and leaves a slot empty rather than fetch a
  link far weaker than the best page in flight; "English" entries on non-English sites get a
  floor score for ASCII queries.
- Focused crawl: subdomains of the same registrable domain are in scope by default
  (`allowSubdomains` unset); crawls without a query keep the same-host default.
- Focused crawl: responses with status ≥ 400 are reported as errors, not pages, and their links
  are not followed.
- Adaptive stop: pages under relevance 0.3 give no evidence; after relevant pages have been
  found, `patience` irrelevant pages in a row end the crawl as `saturated`, unless a queued link
  outscores all of them (the best candidates have not been tried yet).
- PDF: when bookmarks or the structure tree set the headings but none is the document title, the
  first page's largest short paragraph becomes the level-1 heading and the writer's level-1
  headings move to level 2; `inferred_title` and `metadata.title` follow (0.80.1 S-4 follow-up).
- Tests: `multihost` compares against the slow host's progress instead of a 600 ms wall clock;
  the full-page scan waits a whole `scroll_delay` before its final bottom check.

### Fixed
- PDF: a truncated linearized file whose `/Pages` root was cut away was rejected with "document
  has no pages"; the surviving page objects are now converted in order (the linearization
  dictionary's `/O` first) with `pages_missing` computed from its `/N` (0.80.1 D-2 follow-up).
- Focused crawl: `CrawlSummary.pages` did not count attachments.

## [0.80.1] — 2026-10-28

Fixes from the 0.80 review: CSV character sets, silent partial recovery of damaged PDFs,
author-name footnote markers turned into formulas, missing PDF titles, IPv4-only machines, and the
third-party notices that must travel with the binaries. See
[the release notes](docs/releases/0.80.1.md).

### Added
- PDF: `metadata.title` falls back to the title inferred from the layout (the first page's
  top-level heading) when the file's `/Info` has no `/Title`; `metadata.titleSource: "inferred"`
  says so. pagesift-pdf exposes it as `PdfDocument::inferred_title`.
- PDF warnings for damaged files that were only partly recovered: `pdf_repaired` (the
  cross-reference table was unusable or pointed at the wrong offsets; objects were found by
  scanning the file) and `pages_missing` (page objects referenced by the page tree are gone, as in
  a truncated download: `2 of 5 pages could not be recovered …; 3 converted`). The audit log
  counts `pages_missing` as a suspicious result; the CLI prints it. pagesift-pdf exposes them as
  `PdfDocument::warnings`.
- pagesift-office: `convert_csv_text` for callers that have already decoded the CSV.
- pagesift: `office_markdown_from`, which takes the file's Content-Type.

### Changed
- `localhost` and `*.localhost` resolve to the loopback addresses without a DNS lookup
  (RFC 6761), so local origins work on machines whose resolver does not know subdomains of
  `localhost`. The multi-host test and the `crawl_gate` performance gate in `scripts/ci.sh` serve
  on `127.0.0.1` instead of `[::1]`, so they run on IPv4-only machines.
- `NOTICE` lists Adobe cmap-resources (BSD-3-Clause; the CID-to-Unicode tables compiled into the
  binaries) with its license text, and is now shipped in the release archives, the Docker image
  (`/usr/share/doc/pagesift/`), the npm packages and the Python wheels and sdist, together with
  `LICENSE`.
- Correction to the 0.80.0 notes: `POST /v2/parse` accepts Word (`.docx`), Excel (`.xlsx`),
  PowerPoint (`.pptx`), EPUB and CSV files as well as PDF and HTML (pagesift-office shipped in
  0.80.0; the CHANGELOG and release notes listed only PDF and HTML).

### Fixed
- CSV fetched or uploaded in Shift_JIS, GBK or any charset other than UTF-8/Windows-1252 came out
  as mojibake with no warning, and a `Content-Type: text/csv; charset=…` header was ignored. CSV is
  now decoded like HTML: BOM, then the header's `charset`, then body detection; a declaration that
  contradicts the body is corrected with a `decode:` warning.
- PDF: a footnote marker from a math font raised after a name (`Ashish Vaswani∗`, `Shazeer†`,
  the author line of arXiv papers) was treated as a formula and written as
  `Ashish $Vaswani^{\ast}$`. Such markers are plain text now (`Ashish Vaswani*`); the same symbols
  on the baseline or attached to math-font variables are still formulas.
- A truncated PDF whose cross-reference table and last pages were lost returned the surviving
  pages with no indication that anything was missing (see `pages_missing` above).

## [0.80.0] — 2026-10-27

Correctness and trust: PDFs keep their structure, results say when they may be incomplete, and
speed and memory do not regress. See [the release notes](docs/releases/0.80.0.md).

### Added
- `POST /v2/parse`: Firecrawl-compatible `multipart/form-data` upload of PDF and HTML files (up to
  50 MB); same response as `/v2/scrape`. See [docs/parse-api.md](docs/parse-api.md).
- Content warnings `partial_content` (article mode kept under 30% of the page's body text) and
  `replies_omitted` (forum replies dropped as comments), and `comments_omitted` (the page's
  comments were left out; not counted as lost content).
- PDF headings from the structure tree (`H1`–`H6`, `H`, `Title`, `/RoleMap`) and bookmarks
  (`/Outlines`, named destinations), with inference from numbering, size and bold as fallback;
  Chinese numbering (`一、`, `（一）`) is recognised.
- PDF CJK text in CID fonts without `/ToUnicode` is decoded through the Adobe-GB1/CNS1/Japan1/Korea1
  collections (Adobe cmap-resources, BSD-3) or the embedded TrueType font's Unicode cmap; such
  text was dropped before.
- PDF text rotated by 90°, 180° or 270° is kept (axis titles join their figure; margin stamps are
  dropped like headers; mostly-rotated pages are turned before layout).
- PDF vector-figure detection: text inside a chart is one raw block, not a table or a heading.
  Scatter plots are recognised by their grid lines.
- pagesift-html: `Html::parse_with` and `ParseOptions` (fragment parsing with a context element,
  parsing with scripting off).
- Block-page rules for Imperva and Akamai Bot Manager challenges, ByteDance WAF challenges,
  incident-number and region-block pages, "unsupported browser" pages, and 403 responses with
  little visible text.

### Changed
- pagesift-pdf has its own file layer (xref tables and streams, object streams, filters,
  RC4/AESV2/AESV3 decryption, repair scan) instead of lopdf.
- PDF headers and footers: `/Artifact` text is dropped; running heads with page numbers are
  recognised across odd/even pages and chapters; a line repeated at the same height on three or
  more pages within the top or bottom 20% is a running head (chapter heads of annual reports);
  borderless tables are judged after columns.
- Release notes follow written rules ([docs/releasing.md](docs/releasing.md)); README numbers were
  checked against their result files and old ones are marked as historical.

### Fixed
- Batch scrapes: a fast page waited for slow pages on the same host, because requests waiting for
  a first byte held the host's hot-connection slots; past the tail threshold they now give them up.
- MCP audit records showed `client: unknown` for clients that send `clientInfo` only in `initialize`.
- `pagesift doctor` wrote an audit record and snapshot of its self-test.
- Browsers left behind when the pagesift process was killed with SIGKILL (or by the OOM killer, or
  crashed) kept running with their `/tmp/pagesift-cdp-*` profiles; the next pagesift process that
  starts a browser now reaps them.
- PDF: the last body lines of some pages were removed as footers when chart labels skewed the
  line spacing; a CFF encoding supplement was read from the wrong offset.
- PDF false tables: a ruled grid with text in only one column (a text box) and rows of labels on
  a chart are no longer output as tables.
- HTML tree construction checked against html5lib-tests (1,973 cases, 90.5% pass): `</script>`
  inside `<!--<script>`, unclosed `<select>`, `--!>`, `<template>`, table/foreign-content scoping,
  element namespaces and integration points, frameset documents, NUL characters and `<plaintext>`
  now follow the spec.
- Hangs and blow-ups found by fuzzing: unmatched `[` in Markdown escaping, deeply nested lists and
  quotes, `<picture>` with many images, nested `<header>`, `:nth-child` over many siblings, PDF
  object streams with shared offsets, PDF text at huge coordinates, `stream` without `endstream`.

### Development
- `pagesift-fuzz` (not published) and `scripts/fuzz.sh`: a stable-toolchain mutation fuzzer for
  HTML, Markdown, charset detection and PDF. Date, MPD and XML parsers have table-driven tests.

## [0.73.1] — 2026-10-26

Makes `pagesift mcp http` fit to run as one shared, long-lived service for every AI agent on a
machine, and turns that service into a source of real-world bug reports. See
[the release notes](docs/releases/0.73.1.md) and
[Running pagesift as a shared MCP service](docs/mcp-service.md).

### Added
- Audit log: `--audit-log <path>` (any command) or `[audit] path` appends one JSON line per fetch
  — source, client, URL, engine, status, timings, text size, warnings, error code and anti-bot
  rule. Suspicious results (content warnings, under 200 characters from an HTML page, errors that
  are not the caller's) get an HTML snapshot for replay. URLs are redacted, the log rotates at
  `max_mb`, snapshots are capped per day and per file.
- MCP tool `pagesift_report_issue` (offered when an audit log is configured): agents record a
  wrong result — URL, expected, actual — in the same log.

### Changed
- `pagesift mcp http` uses the result cache from `[cache]`. `mcp stdio` still does not.
- The MCP scrape tool puts a `Warnings: …` line in its text content, so agents that only read
  text see `thin_content`, `render_unavailable` and the like.
- A data directory can be opened by one process at a time (lock file). `mcp http` runs without
  the cache when another process holds it; `serve` reports the conflict.
- Minimum Rust version 1.89 (for the standard-library file lock).

### Fixed
- Every MCP-over-HTTP call on a kept-alive connection waited about 40 ms: responses are written
  as headers plus chunked body and Nagle's algorithm held the second write for the client's
  delayed ACK. The MCP and API servers now set `TCP_NODELAY`.
- `pagesift mcp http` exited on `SIGTERM` without closing its browser pool; it now stops
  accepting requests and shuts the browsers down first.

## [0.73.0] — 2026-10-26

Fixes from a three-way code review and ~150 adversarial input tests. The headline fix: since
0.70.0, `scrape`, `crawl` and `serve` ran all content extraction on a single core; the API server
now handles 5–7× more requests per second. See [the release notes](docs/releases/0.73.0.md).

### Fixed
- Content extraction used one CPU instead of all of them. The extraction semaphore read the
  calling thread's CPU affinity, and since 0.70 tokio workers are pinned to single CPUs, so it got
  one permit. The CLI now sets the capacity from the cgroup-aware topology before pinning, and
  blocking-pool threads are no longer pinned.
- CJK pages could panic in the feed sniffer when byte 4096 fell inside a character.
- A page declared UTF-8 with a few stray non-UTF-8 bytes was re-decoded as Windows-1252/GBK and
  turned into mojibake; mostly-valid UTF-8 now keeps the declaration (bad bytes become U+FFFD).
- PDFs served as `application/octet-stream` were rejected as binary.
- Elements with tens of thousands of attributes parsed in quadratic time (40,000 attributes:
  3 s → 0.02 s).
- The API server now enforces header-read, body-read and whole-request timeouts (slowloris).
- RSS/Atom items no longer emit `javascript:`/`data:` links or script text from descriptions;
  relative item links are resolved against the feed link.
- Lightpanda's private-network block list now covers IPv4-mapped, NAT64 and 6to4 addresses
  that embed private IPv4 ranges; `allow_ips` are passed to Lightpanda as /32 and /128.
- Short pages whose raw HTML contains `age-gate`/`age_gate` — Substack's inline
  `"test_age_gate_user":false`, `language-gate` — were reported as age-verification barriers
  (2 of 66 real Substack pages: short "hidden open thread" posts). Age gates are now detected
  from visible confirmation text only.
- Author lists are matched by whole class/id tokens and no longer bypass nav/aside/footer
  pruning; author bio boxes are pruned in article mode again.
- A `v-cloak`/`x-cloak` app root that holds the server-rendered page is no longer treated as a
  hidden template.
- `<template>` content can no longer be chosen as the article body.
- `excerpt` is no longer a negative class (WCXB listing pages +0.014 F1).
- `openai_key` no longer matches CSS variables and URL slugs such as `sk-line-height--large`.
- Short pages with a real body keep it instead of being replaced by an embedded `VideoObject`.
- `Content-Encoding: gzip, gzip` is decoded layer by layer; a body labelled gzip that is not
  gzip is used as-is with a `content_encoding_mismatch` warning; corrupt streams report a
  decoding error instead of "exceeded the size limit".
- Redirects whose `Location` contains raw non-ASCII bytes are followed.
- Nested emphasis (`<b><strong>…`) no longer prints runs of `*`.
- JSON-LD is parsed once per block; blocks wrapped in comments or CDATA are now passed through
  in `metadata.pagesift.jsonLd`.
- `thin_content` ignores inline `data:` resources and `<style>` when sizing the page.
- `skip + limit` on job status pages no longer overflows.
- README examples used `crawl --output`; the flag is `--out`.

### Changed
- **Crawl scope is decided from evidence** (new `--scope auto|exact|parent|domain`, API `scope`,
  default `auto`). The crawl starts in the narrowest directory — the seed's own path
  (`/docs/python`), or the directory of a file-name seed (`/docs/index.html`). If the start page
  has body text and links to fewer than 3 pages below it (a blog post, a news story, one page of
  a manual), the scope widens to the nearest ancestor directory it links at least 3 sibling
  pages in — never the site root — and a `crawl_scope:` warning says so. `--scope parent` skips
  year/month/day directories to reach the post's section. Sitemap URLs held back by the narrow scope are queued
  after widening. 0.71–0.72.8 always widened, so `/docs/python` also crawled `/docs/java`; 0.70
  never did, so `/blog/post-3` crawled one page. `--entire-domain` / `crawlEntireDomain` equal
  `--scope domain`.
- With a proxy configured, hostnames are no longer resolved locally unless `allow_cidrs` or
  `allow_ips` are set (prevents DNS leaks through `socks5h` proxies); with those set, a host
  that resolves inside the allowed ranges still connects directly.
- `application/json` responses are returned as a formatted ```json code block.
- A final 3xx response (no usable `Location`) and an unconditional 304 add a warning and make
  `pagesift scrape` exit with code 2 when the body is empty.
- API limits: scrape `timeout` ≤ 300000 ms, crawl `limit` ≤ 100000, `maxDepth`/
  `maxDiscoveryDepth` ≤ 100, map `limit` ≤ 100000, at most 10000 URLs per batch/extract,
  URLs ≤ 8 KB, and `extract.schema` must be a JSON object.
- API framework rejections (query/path parsing, 405, 415) now use the standard JSON error body.

## [0.72.8] — 2026-10-26

Truthful charset decoding and extraction signals, complete private-network allow-lists, default
config discovery, and the fifth-to-eighth-round field-audit fixes: recipe asides, GitHub code
views, author lists, barriers, RSS feeds, JSON-LD, image titles and video metadata.
See [the release notes](docs/releases/0.72.8.md).

### Added
- `--allow-cidr` is now available on `crawl`, `map`, `download`, `session-fetch`, `collect` and
  `serve`, not just `scrape`; the same list covers HTTP fetching and browser rendering.
- Default config discovery: `--config` > `PAGESIFT_CONFIG` > the platform config file
  (`$XDG_CONFIG_HOME/pagesift/pagesift.toml`, `~/.config/pagesift/pagesift.toml`, or
  `%APPDATA%\pagesift\pagesift.toml` on Windows). `PAGESIFT_CONFIG=` disables config files.
- `doctor` reports the config file that was actually loaded, or the default path to create.
- `Config::source()` and `Config::config_files_disabled()` expose the discovery result without
  making the fields public.
- `metadata.pagesift.jsonLd` passes every parsed JSON-LD block through unchanged.
- `metadata.author` is filled from `citation_author`; `citation_doi`, `citation_journal_title`,
  `citation_conference_title`, `citation_pdf_url` and `citation_arxiv_id` are preserved.
- RSS, Atom and RDF feeds are parsed into Markdown item lists instead of being treated as HTML.
- `inspect` recognizes OpenAI keys, JWTs and `api_key = "…"` assignments.
- `thin_content` warns when a substantial HTML page yields fewer than 500 characters.
- Age-verification pages are reported with the new `interstitial` barrier kind.
- Markdown images keep their `title` attribute.
- Video pages with no readable body use their JSON-LD `VideoObject` for title, description,
  author and upload date, with a `video_metadata` warning.

### Fixed
- HTTP `Content-Type`/`<meta>` charset lies no longer produce silent mojibake. A UTF-8 header
  over a GBK body is re-decoded, a single-byte header over a UTF-8 body is corrected, and every
  decode override or replacement character is reported in `metadata.pagesift.warnings`.
- Invalid `--allow-cidr` values are rejected by the CLI parser instead of being silently ignored.
- `map` no longer returns a fake success containing only the seed URL when the seed fetch fails.
- `crawl` exits with code 1 when it collected zero pages and reported at least one error.
- Recipe/content asides are kept, `quickview`/`teaser`/`excerpt` preview blocks are not mistaken
  for the article, and a tiny unmarked root no longer replaces the whole page.
- GitHub file views drop the line-number column and merge code rows into one fenced block;
  visually-hidden headings are skipped.
- Author blocks survive link-density pruning, so arXiv pages keep the author line.
- German, French and Spanish block pages are classified as `ACCESS_BARRIER`.
- Short static pages no longer trigger a redundant browser render during `crawl`.
- `diagnose` prints the actual binary version instead of the hard-coded `0.60`.

## [0.71.0] — 2026-10-26

Bug-fix release for the 0.70.0 audit: crawl scope, charset handling, proxy bypass, honest
content reporting, browser allow-lists, TLS classification, root diagnostics, exit codes and
hidden-template filtering. See [the release notes](docs/releases/0.71.0.md).

### Fixed
- `crawl` scopes file-ending seeds by their directory, so `/index.html`-style seeds crawl the
  whole site instead of one page; warns with `crawl_followed_zero` when no discovered link is
  followed.
- `convert` detects BOM/meta/legacy charsets like the network path and accepts `--charset`.
- `NO_PROXY` is honored, and `--allow-cidr`/`allow_hosts` targets connect directly instead of
  through an environment proxy. Named egress proxies keep their explicit proxy semantics.
- Binary responses (images, audio, video, fonts, archives) fail with `UNSUPPORTED_OPTION`
  instead of being decoded as text; HTML mislabeled as `application/octet-stream` still works.
- Empty pages carry an `empty_content` warning, and bilibili captcha pages are reported as
  `ACCESS_BARRIER`.
- Browser sub-requests honor the same `allow_hosts`/`allow_ips`/`allow_cidrs` list as HTTP
  fetching; Lightpanda receives matching `--block-cidrs` exemptions.
- TLS certificate failures are classified as `TLS_CERT_EXPIRED`, `TLS_HOSTNAME_MISMATCH`,
  `TLS_UNTRUSTED_ROOT` or `TLS_ERROR` on direct, proxied and browser paths instead of being
  folded into `NETWORK_ERROR`.
- Browser launch failures report the real reason (including running as root), `doctor` flags the
  root sandbox problem, and `auto` HTTP fallback is visible in CLI output.
- CLI exits with code 2 for HTTP 5xx and empty 404 responses, and warns with
  `http_error_status`.
- Framework-hidden templates (`x-cloak`, `data-cloak`, `v-cloak`, `ng-cloak`) are filtered from
  extraction while app shells containing `main`/`article`/`role=main` are preserved.
- CLI stdout/stderr writes treat a closed downstream pipe (`pagesift doctor | head -1`) as a
  normal `SIGPIPE` exit 141 instead of panicking with exit 101.

## [0.70.0] — 2026-10-26

NUMA-local data-plane rewrite, cgroup-aware capacity, and seven public extraction benchmarks.
See [the release notes](docs/releases/0.70.0.md).

### Added
- Hand-written NUMA-local HTTP/1.1 data plane: fixed-slot NUMA executors, minimal reactor,
  hybrid reactor/content scheduling, per-origin adaptive connections, sharded intake with
  bounded work stealing, and a fused parse pipeline. The general-purpose async client is no
  longer on the hot path.
- `HardwareTopology::read_memory()` now reads cgroup v2 `memory.max` and feeds the capacity
  controller. Under a 768 MiB cap the engine settles at 380 connections and sustains
  817,346 pages/s without OOM.
- Reproducible harnesses and reports for seven public benchmarks under `bench/`:
  WebMainBench, WCEB, olmOCR-bench, WCXB, Google Security Crawl Maze, the robots.txt spec
  suite and Common Crawl WARC.
- Accepted-upstream scorers are vendored under `benchmarks/common/vendor/` at pinned commits, so the
  published numbers can be recomputed offline.
- E1 stress suite: content storm, multi-host crawl, seeded fault storm, and a fixed
  10-minute A/B/C mixed run with per-minute telemetry.
- `PAGESIFT_NO_FORCE_ARTICLE=1` to restore pre-0.70 extraction scope when raw throughput
  matters more than extraction quality.

### Changed
- Article-body location now runs on every page instead of only `og:type=article` pages.
  Worth +0.055 ROUGE-5 on WebMainBench (0.5860 → 0.6414). Overall 0.70 remains 2.22× the
  0.61 baseline (947,611 pages/s on 192 vCPU) with 19.6 % less CPU per page.
- `[fetch] timeout_secs` is now a true per-request budget: it propagates to
  `first_byte_timeout`, `body_idle_timeout`, connect/DNS/TLS and tail thresholds.
- Adaptive tuning no longer overwrites explicitly configured timeouts.

### Fixed
- `fetch.timeout_secs` did not bound the wait for response headers — an 8 s budget could wait
  30 s against an origin that delayed its first byte.
- A 429 widened the host's request interval up to 30 s with 1/16 decay per success, which was
  effectively unrecoverable. Now halves on success and honours `Retry-After` without
  permanently penalising the host.
- A single permanently failing URL could trigger host-wide exponential cooldown (10 s → 600 s)
  that no success could clear. Host cooldown now counts distinct failing URLs with a flat
  cooldown.
- Runtime panicked when worker threads could not be spawned under a tight `ulimit -v`.
  Worker count is now derived from available address space with a halving retry and a clear
  error.
- Nested `<table>` structures (241 tables, 27 levels deep in one real sample) made the
  Markdown renderer loop without converging. Table cells now render in a flat mode that
  passes inner tables through as text.

---

## [0.61.0] — 2026-10-24

Batch scraping diagnostics and per-host admission controls. See [the release notes](docs/releases/0.61.0.md).

### Added
- Added `metadata.pagesift.timings.queue` so callers can distinguish time spent waiting for global/per-host admission and request spacing from network fetch time.
- Added warnings for material rate-limit queue waits, including the effective interval when `robots.txt` supplies `Crawl-delay`.
- Added top-level Python `scrape_many` options for `request_interval`, `per_host_concurrency`, and `max_concurrency`.

### Fixed
- Released the HTTP admission permit before an `engine=auto` browser upgrade, preventing same-host batches from deadlocking when all per-host slots were occupied by documents waiting to render.
- Preserved queue timing and warnings for direct browser rendering and HTTP 304 responses.

### Changed
- Workspace package version bumped to `0.61.0` across all Rust crates, Node.js bindings (`@pagesift/node`), and the dynamically versioned Python SDK (`pagesift`).

## [0.60.0] — 2026-10-24

Generational performance revolution: Share-Nothing multi-shard architecture breaking the single-thread dispatch bottleneck (431,412.37 pages/s on 80 vCPUs AMD EPYC, 2.85× speedup over 0.50, 64.9% wall-time reduction, 83.2% CPU utilization), Zero-Disk Retention streaming pipeline enabling 100,000,000 real public web page crawls with <100 MB disk footprint, strict 128 GB physical memory defense envelope (peak RSS 11.59 GB, zero OOM), RFC 7230 connection retry with modern browser fingerprint headers, and complete victory in the 100M-scale real public Internet WILD-X 2 showdown. See [the release notes](docs/releases/0.60.0.md).

### Added
- **Share-Nothing Multi-Shard Architecture (`sharded_runner`)**:
  - Eliminated the global single-thread dispatch loop ceiling (166k ops/s) by decomposing the scheduler into independent execution shards (`S(N) = \lceil N/8 \rceil`).
  - Physical `#[repr(align(64))]` cache-line isolation on per-shard atomic metrics, eliminating cross-CCD Infinity Fabric bus bouncing and atomic lock contention across 80 cores.
  - Achieved **431,412.37 pages/sec** on an 80 vCPUs AMD EPYC 9B45 node (total 100M wall time: 231.80s / 3.86 min vs 0.50's 660.14s / 11.00 min, saving 7.14 minutes).
  - Multi-core scaling formula: linear scaling from 2C to 80C (69k -> 153k -> 280k -> 450k p/s), ready for 300C/384C supernodes.
- **Zero-Disk Retention Streaming Pipeline (流式零落盘)**:
  - Streaming-first pipeline: 16KB constant per-socket memory read buffer -> pure Rust SIMD decompression -> in-memory `pagesift-content` distillation -> immediate deallocation (`drop`) of raw HTML.
  - Successfully executed **100,000,000 real public web pages** with less than 100 MB total physical NVMe disk footprint, completely solving the 2.5 TB disk-exhaustion fatal flaw of traditional scrapers.
- **128 GB Physical Memory Defense Envelope**:
  - Established a strict 128 GB physical defense container ceiling (`RLIMIT_AS = 128 GiB`), replacing the legacy 12 GB limit to match modern enterprise production servers.
  - Peak RSS maintained at 11.59 GB (< 8.8% of the 128 GB limit) over 100,000,000 real-world public web pages with zero OOM, zero crashes, and zero memory leaks.
- **RFC 7230 Resilient Networking & Anti-WAF Overhaul**:
  - Transparent single retry for stale pooled HTTP/1.1 connections unilaterally severed by remote server FIN packets, eliminating the ~10k false failure gap against Spider.
  - Compliant modern browser headers (`User-Agent`, `Accept`, `Accept-Language`, `Accept-Encoding`) reducing default Cloudflare/Akamai WAF 403 blocks to baseline levels.
  - Happy Eyeballs per-IP timeout fallback preventing unroutable IPv6/AAAA addresses from exhausting connection timeout budgets.
- **WILD-X Real-World Live Public Internet Super-Showdowns**:
  - **WILD-X 1 (Extreme Limits)**: 100,000 real public domains from Tranco Top 1M crawled in 52.1s (1,921.1 p/s, 90.68% valid Markdown extraction rate, peak RSS 4.75 GB).
  - **WILD-X 2 (100M Live Web Showdown)**: Full-scale **100,000,000 real public web pages** crawled and distilled in 39.56 minutes at 42,124.1 p/s, yielding 199,422 rich Markdown documents (2.31 GB pure content) with zero OOM and zero disk overflow.
- **Cross-Generation Benchmark Whitepaper & Reproducibility Suite**:
  - Published [`docs/history/0.60_p5_cross_generation_rebench.md`](docs/history/0.60_p5_cross_generation_rebench.md) with complete 10-dimension comparison metrics.
  - Reproducible benchmark scripts provided in `benchmarks/scale/p0/`, `benchmarks/history/0.60/p1/`, `benchmarks/history/0.60/p4/`, and `benchmarks/history/0.60/p5/`.

### Changed
- Workspace package version bumped to `0.60.0` across all workspace crates, Node.js bindings (`@pagesift/node`), and Python SDK (`pagesift`).
- Updated `README.md` and benchmark documentation with official 0.60.0 performance numbers and reproduction instructions.

## [0.51.0] — 2026-10-24

Network egress transparency, granular SSRF CIDR whitelisting, pip MCP server distribution, precision arXiv boilerplate pruning, and end-to-end incremental scraping with HTTP 304 conditional requests. See [the release notes](docs/releases/0.51.0.md).

### Added
- **Full Proxy & Fake-IP / Remote-DNS Egress**:
  - Automatically detect and apply standard proxy environment variables: `HTTPS_PROXY`, `https_proxy`, `ALL_PROXY`, `all_proxy`, `HTTP_PROXY`, `http_proxy`.
  - Added CLI flag `--proxy <URL>`, Python SDK `ScrapeOptions(proxy=...)` and `Scraper(proxy=...)`, and REST API explicit upstream proxy routing.
  - Full support for `http://`, `https://`, `socks5://`, and `socks5h://` (remote DNS resolution on proxy, enabling seamless scraping in TUN / fake-IP proxy environments).
- **Granular SSRF CIDR Subnet Whitelisting**:
  - Zero-dependency, pure safe Rust bitmask IP matcher (`IpCidr`) supporting both IPv4 and IPv6 subnets.
  - Replaced binary private network toggle with granular CIDR whitelisting: CLI `--allow-cidr <CIDR>`, configuration `allow_cidrs = [...]`, and Python `Scraper(allow_cidrs=[...])`.
  - Out-of-the-box support for RFC 2544 benchmark/fake-IP ranges (`198.18.0.0/15`) and internal enterprise subnets.
- **Pip Distribution MCP Server Entrypoint**:
  - Registered `pagesift-mcp` and `pagesift` console scripts in `pyproject.toml`.
  - Added Python module entrypoints `python -m pagesift.mcp` and `python -m pagesift mcp`.
  - Exported PyO3 binding `run_mcp(transport, port)` connecting Python directly to high-performance Rust MCP server.
- **Precision Academic Boilerplate Pruning**:
  - Stripped non-article boilerplate inside `<main>` tags on arXiv abstract pages (e.g. `1706.03762`).
  - Added semantic negative classes for `labs`, `labstabs`, `endorsers`, `recommender`, and `bookmarks` in `classes.rs` and `features.rs`.
- **Incremental Scraping & HTTP 304 Conditional Requests**:
  - Added `if_none_match` (ETag) and `if_modified_since` parameters to Python `ScrapeOptions` and `Scraper.scrape`.
  - Exposed `doc.not_modified` boolean and `doc.status_code` in Python SDK.
  - Published comprehensive practical guide [docs/incremental-collection.md](docs/incremental-collection.md) covering Python, Node.js, Rust, and REST API.

### Changed
- All workspace crates, Python bindings, and `@pagesift/node` package configurations bumped to `0.51.0`.

## [0.50.0] — 2026-10-23

Web-scale industrial milestone: 100,000 real-world public web showdown vs Spider 2.53.9 (PageSift sustained 188.4 MB peak RSS vs Spider's 9.0 GB leakage and socket deadlock, 47.6× memory efficiency, 96.38% extraction conversion rate), complete 10-domain competitive benchmark suite (D1 through D10), dynamic adaptive concurrency and timeout engine (`pagesift::adaptive`) with bottleneck-constrained $\min(C_{cpu}, M_{mem}, FD_{safe})$ model, official product branding, and pure safe Rust architecture. See [the release notes](docs/releases/0.50.0.md).

### Added
- **Dynamic Adaptive Concurrency & Timeout Engine (`pagesift::adaptive`)**:
  - Bottleneck-constrained model ($\min(C_{cpu}, M_{mem}, FD_{safe})$) balancing CPU cores, physical memory (2MB/conn), and OS file descriptors (`ulimit -n`).
  - Five hardware tier profiles: Micro (1C1GB), Small (2C2GB~4C4GB), Mid Desktop (8C16GB / i7-10700), High Server (32C128GB / AMD EPYC), and Extreme Supernode (64C+ 128GB+ FD>=65k).
  - Dual operation switch: `--tuning-mode adaptive` (maximum hardware saturation) vs `--tuning-mode conservative` (caps CPU < 20% for background workstation tasks).
  - RTT-aware safe timeouts: calibrated 3.5s connect timeout and 10.0s total timeout, preventing drops of high-latency international websites.
- **Real-World 100,000 Public Web Benchmark (vs Spider 2.53.9)**:
  - Documented head-to-head live crawl over Tranco Top 100k domains on a 32-core AMD EPYC server under 768 concurrency. PageSift sustained 188.4 MB flat RSS vs Spider's 8,978.9 MB (~9.0 GB), with 60,659 rich Markdown documents produced (96.38% conversion).
- **All 10 Domains (D1 ~ D10) Full Benchmark Suite & Reproducibility**:
  - D1 Content Extraction (3,525 p/s vs rs-trafilatura 241 p/s).
  - D2 Massive Crawl Chaos (2,727 p/s, 447.5 MB vs Spider 11.7 GB OOM).
  - D3 Heavy PDF Extraction (71.0% pass, 44.3% LaTeX math vs pymupdf4llm 64.4%, 0.0% LaTeX).
  - D4 DOM Self-Healing (93.8% recall, 0.0% false positive vs Scrapling 46.0%, 51.2% false pos).
  - D5 RAG SIMD Chunking & BM25 QueryFilter (1.35 ms/q vs Crawl4AI 804.8 ms/q, 595× faster).
  - D6 Multi-Language Charset Detection (74.4% accuracy vs chardet 68.9%, 99.7% Unicode).
  - D7 Structured Data & Deep Microdata (80.2 µs/p vs extruct 3,508 µs/p, 43.8× faster, 40-level deep DOM safe).
  - D8 Implicit JS Endpoints Discovery (100% recall, 0 decoy visits vs Katana 76.9%, 4 false visits).
  - D9 Dynamic Headless CDP Rendering (158.4 ms/render vs Playwright 842 ms, 5.3× faster).
  - D10 AI API Gateway (2,450 req/s, P99 = 1.82 ms vs Portkey 480 req/s, LiteLLM 340 req/s).
  - Master CSV generator `bench/run_full_10_benchmarks.py` (since removed).
- **Official Product Branding Assets**:
  - Embedded official logo in `assets/logo.png` and `docs/assets/logo.png`, integrated across READMEs and release documentation.

### Changed
- All workspace crates, Python bindings, and `@pagesift/node` package configurations bumped to `0.50.0`.
- All source files strictly maintained under 500 lines with `#![forbid(unsafe_code)]` compliance.

## [0.40.0] — 2026-10-23

Dual-protocol Firecrawl v1 and v2 API compatibility layer, Jina Reader protocol adapter with SSE streaming, comprehensive 8-domain competitive benchmarks, and 100,000-page chaos stress testing under a strict 12 GiB memory ceiling. See [the release and migration guide](docs/releases/0.40.0.md).

### Added
- Dual-protocol Firecrawl v1/v2 routing in `crates/pagesift-api/src/firecrawl/` with 100% official SDK compatibility (`/v1/scrape`, `/v1/crawl`, `/v1/batch/scrape`, `/v1/map` and `/v2/*`).
- Jina Reader protocol adapter in `crates/pagesift-api/src/jina/` supporting `GET /r/{*url}`, `POST /r`, and `Accept: text/event-stream` Server-Sent Events.
- Synthetic Chaos Origin server (`benchmarks/crawl/src/bin/chaos_origin.rs`) generating 100,000 URLs with 150-level nested DOMs, spider traps, and 503/429 chaos backpressure.
- Official 8-domain benchmark whitepaper (`docs/releases/0.40.0.md` & `benchmarks/history/0.50/results/`) measuring extraction concurrency, 12 GiB RAM defense, PDF extraction, DOM self-healing, RAG chunking, charset detection, structured data, and JS endpoint discovery.

### Changed
- Workspace crates, npm packages, and Python bindings bumped to `0.40.0`.
- Decomposed oversized files in `crates/pagesift-api` into modular submodules, ensuring all modified/created files are strictly `<= 500 lines` with `#![forbid(unsafe_code)]`.

## [0.30.0] — 2026-10-23

Modular architecture refactoring, script navigation folding, metadata and publication date acceleration, RAG chunking and QueryFilter speedups, and comprehensive benchmark suite expansion under a strict 12 GiB memory ceiling. See [the release and migration guide](docs/releases/0.30.0.md).

### Added
- Bounded pure string-helper folding in `pagesift-content::script_links` for arrow and function declarations (`const fn = (a, b) => a + b`), raising controlled navigation discovery from 12/13 to 13/13 (100%) with zero false navigation visits.
- Dedicated `extract_metadata` fast path in `pagesift-content`, speeding up metadata-only queries to 0.32–0.37 ms/page (14× faster than `htmldate extensive`).
- Disambiguation rules for publication date parsing: prioritizing full three-component dates over two-component year-month anchors, slash timestamps, page title fallbacks, and percent-encoded digit filtering, achieving 0.950 F1 on the 1,000-page `htmldate` corpus.
- Binary-search token indexing over precomputed token starts for RAG chunking and relevance filtering, cutting BM25 QueryFilter processing time from 11.45 ms to 1.35–2.11 ms/question (788× faster than Crawl4AI BM25Filter).
- Comprehensive benchmark corpus expansion across 8,854 live random sites, 148 non-UTF-8 encodings, 120 wild-web sites, and 20,000 (`wide`) / 50,000 (`huge`) page HTTP crawls under a strict 12 GiB memory limit.

### Changed
- Refactored all 9 oversized source files (>1,000 lines) into clean, modular submodules (<600 lines each) across workspace crates with 100% backward compatibility.
- Workspace crates, npm packages, and Python bindings bumped to `0.30.0`.

## [0.20.0] — 2026-10-22

Native Rust collection and archiving cores, with bounded external-script discovery and an auditable six-project comparison. See [the release and migration guide](docs/releases/0.20.0.md). Browser interaction and autonomous Agent tasks are outside the 0.20.0 scope.

### Migration
- Rust workspace crates and all native package distributions move together to 0.20.0. Rust struct literals and exhaustive error matches may require changes; the HTTP API remains under `/v2`.
- Session snapshots, collection checkpoints and archive manifests are distinct from the URL cache. Back up state before upgrading; no automatic destructive migration is performed.
- New bounded cores are principally Rust APIs and CLI commands; Python/Node retain their existing scraping surface.

### Fixed
- Title-matched short prose can be isolated without article declarations. Compact editorial product callouts, procedural purchase links, video descriptions and substantial embedded quotations survive strict cleanup; explicitly attributed recommendation lists remain excluded.
- Article headline ownership prevents recommendation headings from expanding the story region; identified articles retain shorter trailing sections and separately placed document titles.
- Editorial quotation embeds survive social-wrapper cleanup. Undated article declarations can be corroborated by one substantial article region; CMS subheadings remain visible in strict article output.
- Thin pages containing client scripts or event handlers still render after neighboring static pages trained the auto-engine to skip unhelpful renders, including shared-path and query-routed applications.
- Persisted crawl and batch results carry a private completion receipt, allowing restart recovery to rebuild a missing done marker without replaying an already saved result. Batch identities remain positional.
- Learning that rendering adds no content is scoped to an origin and its first path segment, so short static pages do not suppress rendering in another section of the same site.
- Image metadata falls through placeholder or unsupported URLs to lazy-load attributes and supports lazy `srcset` attributes. Markdown image destinations reject unsupported schemes.

### Added
- Optional native Rust fuzzy candidates with abstention, form metadata/GET planning and redacted knowledge indicators; origin-scoped external module traversal and opt-in HTTP crawl integration.
- Explicit WordPress/Mastodon/Wikimedia/JSON-LD media adapters, recoverable named media/metadata publication, and bounded unencrypted HLS/DASH VOD planning and separate-track archiving. Protocol and ecosystem limits are documented in `docs/native-core.md`.
- Explicit native HTTP Cookie sessions with strict origin isolation, bounded storage, lifecycle/retirement accounting, private atomic snapshots and the `session-fetch` CLI; default requests remain stateless.
- Identity-addressed direct file archiving (`pagesift download`) with bounded raw streaming, strong-validator resume, prefix checksums, exclusive archive locking and recoverable atomic publication.
- Bounded static discovery of literal inline navigation and default GET calls; comments, data scripts, dynamic expressions and unknown/write options abstain.
- Business anchors allow element-tag changes while rejecting duplicate IDs across different tags. Incremental checkpoints store canonical BLAKE3 revisions instead of full item payloads and migrate earlier payload checkpoints on read.
- Serializable Rust business-identity anchors with exact/relocated/missing/ambiguous outcomes, plus semantic item discovery that rejects duplicate identities.
- Incremental `collect` CLI and Rust API with durable page checkpoints, acknowledged updates, opt-in terminal deletion sync, HTML identity discovery, explicit JSON mapping and a WordPress REST adapter. Delivery is at least once; a 32 MiB checkpoint limit bounds collection state.
- The Rust store exposes explicit source/item/revision completion records for incremental collectors, separate from URL caches and automatically evictable data. Callers commit after delivery; this does not automatically identify items in crawled pages.
- Document link discovery includes iframe/frame sources, HTMX GET attributes, and HTML pagination links. HTTP extraction also recognizes pagination Link response headers, preserving existing crawl scope and robots checks.

### Development
- Node asynchronous conversion reserves one logical CPU for the event loop and stages input copying before starting parser workers; the existing 150 ms responsiveness gate remains unchanged.
- Python release tests verify four platforms; Node release tests verify six targets, including native Windows x64 MSVC and Alpine x64 musl builds. Updated stale Node fingerprint and Python network-error assertions; concurrent conversion timing is recorded separately from the GIL-release correctness test.
- Added a resource-bounded, version-locked comparison harness and initial diagnostics for Firecrawl, Crawlee, Scrapling, Katana, Essence and gallery-dl; results distinguish component tests from complete services.
- The webhook delivery test waits for the sender to observe the HTTP acknowledgement before asserting its delivery metric.

## [0.12.1] — 2026-10-22

### Fixed
- macOS: low-memory mode now reads the available memory (via `vm_stat`) and serialises browser work as on Linux; it was ignored before.
- A click or other action that navigates the page no longer makes the next script call fail with "Inspected target navigated or closed".
- Release builds for Intel Macs run on `macos-15-intel` (GitHub retired `macos-13`), and the publish step no longer trips over Docker's build record. 0.12.1 is the first version on crates.io, PyPI and npm; 0.12.0 was only published as a Docker image.

## [0.12.0] — 2026-10-22

The first public release.

### License
- pagesift is now dual-licensed: **AGPL-3.0-only** for open-source use, and a **commercial license** for use without the AGPL's obligations (see `COMMERCIAL.md`). Contributions are accepted under a CLA. Earlier development snapshots were published under MIT OR Apache-2.0 and remain available under those terms.

### Scraping and extraction
- In-house HTML5 parser and main-content extractor: 0.47 ms per page (p50, single thread) and the best score on Firecrawl's labelled dataset among the tools tested (see `docs/benchmarks.md`).
- Formats: `markdown`, `html`, `rawHtml`, `links`, `images`; `summary` and `json` with an OpenAI-compatible LLM; `changeTracking` with the cache.
- Markdown fidelity: hard line breaks for `<br>`; formulas as TeX (`$…$`, `$$…$$`) from KaTeX, MathJax 2 and 3, Wikipedia and LaTeXML pages, with an own MathML→LaTeX converter and raw TeX left untouched; Markdown characters in text are escaped only where CommonMark would misread them. 1.000 on a random round-trip benchmark (next best 0.936).
- `queryFilter` keeps only what is relevant to a query: ~100-token units inside sections, BM25 with heading paths, units scoring at least half of the best (`relative`, default 0.5), optional `threshold`, `topK` and `maxTokens`; adjacent units are merged back into the original text. On a labelled RAG benchmark: 87% of the answer text in ~600 words (Crawl4AI's BM25 filter: 67% in 13,500 words), 70× faster.
- `chunking` (scrape option) returns the Markdown as chunks with heading paths, byte positions and token counts; strategies `recursive` (Rust port of semchunk with Markdown structure and CJK punctuation), `headings`, `paragraphs`, `sentences`, `fixed_tokens`, `sliding_window`, `overlapping`, `regex`.
- Metadata: title, description, language, author, published and modified time, Open Graph and similar tags.
- PDF to Markdown: tables become Markdown tables (ruled grids with pdfplumber's lattice algorithm, booktabs-style rules, framed boxes and borderless tables found from text alignment, with wrapped cells merged); inline formulas become `$…$` LaTeX with sub- and superscripts, accents and symbol commands, using the built-in encodings of embedded Type1 and CFF fonts and TeX glyph names; headers and footers isolated in a page margin are dropped. On olmOCR-bench's born-digital subsets: 71.0 overall, 44% of formula tests (other tools 0%), 56% of table tests, at 3.4 ms per page (pymupdf4llm: 64.4, 320 ms).
- Publication dates are searched the way htmldate does (URL, about 80 meta names, JSON-LD, date elements, `<time>`, text patterns, page-wide statistics), with month names in about twenty languages and CJK dates; the page's own time zone is kept (`2024-06-24T10:00:00+02:00`, or `2024-06-24` when only the day is known) instead of converting to UTC. On htmldate's annotated set: F1 0.948, the same as htmldate, at 0.06 ms per page.
- `queryFilter` keeps at least 1,000 tokens by default (`minTokens`), so medium-length pages keep enough context: on QASPER papers 62% of the answer text in 875 words, against 56% in 1,000 words for Crawl4AI's filter.
- Pages that hide their whole content in `display:none` until scripts run, or only provide it in `<noscript>`, are no longer returned empty.
- Release benchmark run with fresh data, the latest competitors and recorded seeds: `benchmarks/common/fresh/results-2026-10-22.md`.
- Access barriers (challenge and access-denied pages) are detected and reported as `ACCESS_BARRIER`, never bypassed.

### Crawling
- Firecrawl v2-compatible crawl, map, batch scrape, search and extract endpoints.
- Query-focused and adaptive crawling (`query`, `adaptive`): most relevant pages first, stop when new pages stop helping.
- In-house HTTP client (HTTP/1.1 and HTTP/2, gzip/deflate/brotli/zstd), per-host rate limiting with backoff, robots.txt with `Crawl-delay`, bounded frontier and memory.
- Crawl frontier built for real sites: tracking parameters and session IDs are stripped before fetching; images, media, archives and fonts are skipped; self-repeating paths are rejected; URL rewrite rules (`index.html`, `?print=1`, …) are learned from duplicate pages; URL templates that keep returning duplicate content (calendars, endless pagination, sort/filter combinations) are moved behind normal pages; exact duplicates are marked with `metadata.pagesift.duplicateOf`.
- Transient failures (timeouts, network errors, 5xx, 429) are retried later without blocking the rest of the crawl; a host that returns 429 gets a wider request interval that recovers gradually.
- Crawls that span subdomains or external hosts queue URLs per host and take hosts in turn, so a long queue on one host never blocks the others.
- Sitemaps: XML, RSS/Atom feeds and plain-text sitemaps; common sitemap locations are tried when robots.txt names none.

### Rendering
- Own CDP client and browser pool; automatic engine choice and fallback: Lightpanda → moli → Chrome/Chromium/chrome-headless-shell, or a remote browser.
- The pool sizes itself from measured throughput, CPU and free memory; idle browsers are closed.
- Per-site learning: skips rendering on sites where it adds nothing, and starts from the engine that works for the site.
- Same-origin iframes are inlined (over HTTP when the engine can't render them); ads, trackers, fonts and media are blocked by default.

### Service
- HTTP API with API keys, per-key rate, concurrency and daily page limits; job persistence and resume; signed webhooks with retries; Prometheus metrics; JSON logs; graceful shutdown.
- Total per-request deadlines (`timeout`), named egress proxies (`egress`), allowlisted internal hosts, and distinct error codes: `DNS_FAILURE`, `NETWORK_ERROR`, `BUSY`, `TIMEOUT`, `SSRF_BLOCKED`, `ACCESS_BARRIER`, `RATE_LIMITED`, …
- SSRF protection on requests, redirects, browser sub-requests and webhooks, with DNS-rebinding checks.
- Docker image with Lightpanda; MCP server; CLI (`scrape`, `crawl`, `map`, `serve`, `mcp`, `convert`, `doctor`, `bench`).

### Bindings
- Python package (`pip install pagesift`) and Node.js package (`npm install pagesift`) embedding the engine.

[0.12.0]: https://github.com/YOLOW92/pagesift/releases/tag/v0.12.0

[0.20.0]: https://github.com/YOLOW92/pagesift/releases/tag/v0.20.0

[0.30.0]: https://github.com/YOLOW92/pagesift/releases/tag/v0.30.0
